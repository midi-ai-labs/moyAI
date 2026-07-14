use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::protocol::TurnInterruptionCause;
use crate::runtime::cancel::{RunTerminalRoute, RunTerminalRouteKind};
use crate::runtime::{RunCancelOutcome, RunContinuationOutcome, RunControl};
use crate::session::SessionId;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentPath(String);

impl AgentPath {
    pub const ROOT: &str = "/root";

    const ROOT_SEGMENT: &str = "root";

    pub fn root() -> Self {
        Self(Self::ROOT.to_string())
    }

    pub fn from_string(path: String) -> Result<Self, String> {
        validate_absolute_path(&path)?;
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.as_str() == Self::ROOT
    }

    pub fn name(&self) -> &str {
        if self.is_root() {
            return Self::ROOT_SEGMENT;
        }
        self.as_str()
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or(Self::ROOT_SEGMENT)
    }

    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        self.as_str()
            .rsplit_once('/')
            .and_then(|(parent, _)| Self::try_from(parent).ok())
    }

    pub fn join(&self, task_name: &str) -> Result<Self, String> {
        validate_task_name(task_name)?;
        Self::from_string(format!("{self}/{task_name}"))
    }

    pub fn resolve(&self, reference: &str) -> Result<Self, String> {
        if reference.is_empty() {
            return Err("agent path must not be empty".to_string());
        }
        if reference == Self::ROOT {
            return Ok(Self::root());
        }
        if reference.starts_with('/') {
            return Self::try_from(reference);
        }

        validate_relative_reference(reference)?;
        Self::from_string(format!("{self}/{reference}"))
    }

    fn is_at_or_below(&self, prefix: &Self) -> bool {
        self == prefix
            || self
                .as_str()
                .strip_prefix(prefix.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

impl TryFrom<String> for AgentPath {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_string(value)
    }
}

impl TryFrom<&str> for AgentPath {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::from_string(value.to_string())
    }
}

impl From<AgentPath> for String {
    fn from(value: AgentPath) -> Self {
        value.0
    }
}

impl FromStr for AgentPath {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl AsRef<str> for AgentPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for AgentPath {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Display for AgentPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn validate_task_name(task_name: &str) -> Result<(), String> {
    if task_name.is_empty() {
        return Err("task_name must not be empty".to_string());
    }
    if task_name == AgentPath::ROOT_SEGMENT {
        return Err("task_name `root` is reserved".to_string());
    }
    if task_name == "." || task_name == ".." {
        return Err(format!("task_name `{task_name}` is reserved"));
    }
    if task_name.contains('/') {
        return Err("task_name must not contain `/`".to_string());
    }
    if !task_name.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
    }) {
        return Err(
            "task_name must use only lowercase letters, digits, and underscores".to_string(),
        );
    }
    Ok(())
}

fn validate_absolute_path(path: &str) -> Result<(), String> {
    let Some(stripped) = path.strip_prefix('/') else {
        return Err("absolute agent paths must start with `/root`".to_string());
    };
    let mut segments = stripped.split('/');
    if segments.next() != Some(AgentPath::ROOT_SEGMENT) {
        return Err("absolute agent paths must start with `/root`".to_string());
    }
    if stripped.ends_with('/') {
        return Err("absolute agent path must not end with `/`".to_string());
    }
    for segment in segments {
        validate_task_name(segment)?;
    }
    Ok(())
}

fn validate_relative_reference(reference: &str) -> Result<(), String> {
    if reference.ends_with('/') {
        return Err("relative agent path must not end with `/`".to_string());
    }
    for segment in reference.split('/') {
        validate_task_name(segment)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    PendingInit,
    Running,
    Interrupted,
    Completed(Option<String>),
    Errored(String),
    Shutdown,
    NotFound,
}

impl AgentStatus {
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Completed(_) | Self::Errored(_) | Self::Shutdown)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMailboxMessage {
    pub author: AgentPath,
    pub recipient: AgentPath,
    pub content: String,
    pub trigger_turn: bool,
}

impl AgentMailboxMessage {
    pub fn new(
        author: AgentPath,
        recipient: AgentPath,
        content: impl Into<String>,
        trigger_turn: bool,
    ) -> Self {
        Self {
            author,
            recipient,
            content: content.into(),
            trigger_turn,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub path: AgentPath,
    pub session_id: SessionId,
    pub parent: Option<AgentPath>,
    pub children: Vec<AgentPath>,
    pub spawn_order: u64,
    pub status: AgentStatus,
    pub last_activity: Option<String>,
    pub is_active: bool,
    pub mailbox_generation: u64,
    pub pending_mail_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTreeSnapshot {
    pub root: AgentPath,
    pub max_concurrent_agents: usize,
    pub active_agent_count: usize,
    pub agents: Vec<AgentSnapshot>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentControlError {
    #[error("max_concurrent_agents must be at least 1")]
    InvalidCapacity,
    #[error("invalid agent path: {0}")]
    InvalidPath(String),
    #[error("agent `{0}` was not found")]
    AgentNotFound(AgentPath),
    #[error("agent `{0}` already exists")]
    AgentAlreadyExists(AgentPath),
    #[error("session {0} is already registered in this agent tree")]
    SessionAlreadyRegistered(SessionId),
    #[error("agent `{0}` already has an active turn")]
    AgentAlreadyActive(AgentPath),
    #[error("agent `{0}` has no active turn to cancel")]
    AgentNotActive(AgentPath),
    #[error("agent limit reached (root included; max {max_concurrent_agents})")]
    AgentLimitReached { max_concurrent_agents: usize },
    #[error("the agent tree has been cancelled")]
    TreeCancelled,
    #[error("mailbox for agent `{0}` closed")]
    MailboxClosed(AgentPath),
    #[error("durable mailbox commit failed: {0}")]
    DurableMailboxCommit(String),
    #[error("agent control lock was poisoned")]
    LockPoisoned,
    #[error("agent `{0}` execution lease is stale")]
    StaleExecution(AgentPath),
    #[error("agent `{0}` still has registered children")]
    AgentHasChildren(AgentPath),
    #[error("the root run control is already owned by a different live agent tree")]
    RunControlOwnedByDifferentTree,
}

#[derive(Clone)]
pub struct AgentControl {
    inner: Arc<AgentControlInner>,
}

struct AgentControlInner {
    tree_control: RunControl,
    root_terminal_router: Arc<RunTerminalRoute>,
    state: Mutex<AgentTreeState>,
    mail_delivery: Mutex<()>,
    activity_tx: watch::Sender<u64>,
}

#[derive(Clone, Copy, Debug)]
struct TreeClassificationResult {
    root_outcome: RunCancelOutcome,
    tree_applied: bool,
    source_matched: bool,
}

impl TreeClassificationResult {
    fn rejected() -> Self {
        Self {
            root_outcome: RunCancelOutcome::Rejected,
            tree_applied: false,
            source_matched: true,
        }
    }

    fn unroutable() -> Self {
        Self {
            root_outcome: RunCancelOutcome::Rejected,
            tree_applied: false,
            source_matched: false,
        }
    }

    fn changed(self) -> bool {
        matches!(self.root_outcome, RunCancelOutcome::Applied) || self.tree_applied
    }
}

struct AgentTreeState {
    max_concurrent_agents: usize,
    agents: HashMap<AgentPath, AgentEntry>,
}

struct AgentEntry {
    session_id: SessionId,
    parent: Option<AgentPath>,
    spawn_order: u64,
    status: AgentStatus,
    last_activity: Option<String>,
    execution_marker: Option<Arc<()>>,
    run_control: RunControl,
    mailbox: VecDeque<AgentMailboxMessage>,
    mailbox_generation: u64,
    trigger_admission_epoch: u64,
    trigger_purge_pending: u32,
    mailbox_activity_tx: watch::Sender<u64>,
}

pub struct AgentExecutionLease {
    control: AgentControl,
    path: AgentPath,
    marker: Arc<()>,
    run_control: RunControl,
}

pub enum AgentRootContinuationOutcome {
    Admitted(AgentExecutionLease),
    Blocked,
    NotReady,
    Invalid,
}

#[must_use]
pub enum AgentMailDeliveryOutcome {
    Enqueued {
        generation: u64,
        scheduled: Vec<AgentExecutionLease>,
    },
    Suppressed,
}

impl AgentControl {
    /// Creates a root-scoped tree and reserves its first execution slot for the root turn.
    /// Keeping the returned lease alive makes the root count toward the concurrency limit.
    pub fn new(
        root_session_id: SessionId,
        max_concurrent_agents: usize,
    ) -> Result<(Self, AgentExecutionLease), AgentControlError> {
        Self::with_root_control(root_session_id, max_concurrent_agents, RunControl::new())
    }

    pub fn with_root_control(
        root_session_id: SessionId,
        max_concurrent_agents: usize,
        root_control: RunControl,
    ) -> Result<(Self, AgentExecutionLease), AgentControlError> {
        if max_concurrent_agents == 0 {
            return Err(AgentControlError::InvalidCapacity);
        }

        let tree_control = RunControl::new();
        let (activity_tx, _) = watch::channel(0);
        let (mailbox_activity_tx, _) = watch::channel(0);
        let root = AgentPath::root();
        let mut agents = HashMap::new();
        agents.insert(
            root.clone(),
            AgentEntry {
                session_id: root_session_id,
                parent: None,
                spawn_order: 0,
                status: AgentStatus::PendingInit,
                last_activity: None,
                execution_marker: None,
                run_control: root_control.clone(),
                mailbox: VecDeque::new(),
                mailbox_generation: 0,
                trigger_admission_epoch: 0,
                trigger_purge_pending: 0,
                mailbox_activity_tx,
            },
        );
        let inner = Arc::new_cyclic(|tree: &std::sync::Weak<AgentControlInner>| {
            let tree = tree.clone();
            let root_terminal_router: Arc<RunTerminalRoute> =
                Arc::new(move |source, kind, cause| {
                    let inner = tree.upgrade()?;
                    AgentControl { inner }.route_terminal_outcome(source, kind, cause)
                });
            AgentControlInner {
                tree_control,
                root_terminal_router,
                state: Mutex::new(AgentTreeState {
                    max_concurrent_agents,
                    agents,
                }),
                mail_delivery: Mutex::new(()),
                activity_tx,
            }
        });
        let control = Self { inner };
        let root_execution = control.try_acquire_execution_with_control(&root, root_control)?;
        Ok((control, root_execution))
    }

    pub fn register_child(
        &self,
        parent: &AgentPath,
        task_name: &str,
        session_id: SessionId,
        initial_activity: Option<String>,
    ) -> Result<(AgentSnapshot, AgentExecutionLease), AgentControlError> {
        let child_path = parent
            .join(task_name)
            .map_err(AgentControlError::InvalidPath)?;
        let mut state = self.lock()?;
        if self.inner.tree_control.is_cancelled() {
            return Err(AgentControlError::TreeCancelled);
        }
        if !state.agents.contains_key(parent) {
            return Err(AgentControlError::AgentNotFound(parent.clone()));
        }
        if state.agents.contains_key(&child_path) {
            return Err(AgentControlError::AgentAlreadyExists(child_path));
        }
        if state
            .agents
            .values()
            .any(|agent| agent.session_id == session_id)
        {
            return Err(AgentControlError::SessionAlreadyRegistered(session_id));
        }
        if active_agent_count(&state) >= state.max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: state.max_concurrent_agents,
            });
        }

        // Entries are retained for the lifetime of the tree, so its current length is the
        // canonical spawn order and no separate sequence counter is needed.
        let spawn_order = state.agents.len() as u64;
        let marker = Arc::new(());
        let run_control = RunControl::new();
        let (mailbox_activity_tx, _) = watch::channel(0);
        state.agents.insert(
            child_path.clone(),
            AgentEntry {
                session_id,
                parent: Some(parent.clone()),
                spawn_order,
                status: AgentStatus::PendingInit,
                last_activity: initial_activity,
                execution_marker: Some(Arc::clone(&marker)),
                run_control: run_control.clone(),
                mailbox: VecDeque::new(),
                mailbox_generation: 0,
                trigger_admission_epoch: 0,
                trigger_purge_pending: 0,
                mailbox_activity_tx,
            },
        );
        let snapshot = snapshot_agent(&state, &child_path)
            .expect("a child inserted into the registry must be available for its snapshot");
        drop(state);
        self.notify_activity();

        Ok((
            snapshot,
            AgentExecutionLease {
                control: self.clone(),
                path: child_path,
                marker,
                run_control,
            },
        ))
    }

    pub fn try_acquire_execution(
        &self,
        path: &AgentPath,
    ) -> Result<AgentExecutionLease, AgentControlError> {
        self.try_acquire_execution_with_control(path, RunControl::new())
    }

    pub fn try_acquire_execution_with_control(
        &self,
        path: &AgentPath,
        run_control: RunControl,
    ) -> Result<AgentExecutionLease, AgentControlError> {
        let mut state = self.lock()?;
        if self.inner.tree_control.is_cancelled() {
            return Err(AgentControlError::TreeCancelled);
        }
        let agent = state
            .agents
            .get(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        if agent.execution_marker.is_some() {
            return Err(AgentControlError::AgentAlreadyActive(path.clone()));
        }
        if active_agent_count(&state) >= state.max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: state.max_concurrent_agents,
            });
        }

        if path.is_root() {
            self.install_root_terminal_router(&run_control)?;
        }

        let marker = Arc::new(());
        let agent = state
            .agents
            .get_mut(path)
            .expect("agent existence was checked while holding the same registry lock");
        agent.execution_marker = Some(Arc::clone(&marker));
        agent.run_control = run_control.clone();
        drop(state);
        self.notify_activity();

        Ok(AgentExecutionLease {
            control: self.clone(),
            path: path.clone(),
            marker,
            run_control,
        })
    }

    pub fn try_acquire_root_continuation(
        &self,
        run_control: RunControl,
    ) -> Result<AgentRootContinuationOutcome, AgentControlError> {
        let root_path = AgentPath::root();
        let mut state = self.lock()?;
        if self.inner.tree_control.is_cancelled() {
            return Ok(AgentRootContinuationOutcome::Blocked);
        }
        let root = state
            .agents
            .get(&root_path)
            .ok_or_else(|| AgentControlError::AgentNotFound(root_path.clone()))?;
        if !root.run_control.same_owner(&run_control) {
            return Ok(AgentRootContinuationOutcome::Invalid);
        }
        if state.agents.values().any(|agent| {
            agent.execution_marker.is_some()
                || agent.mailbox.iter().any(|message| message.trigger_turn)
        }) {
            return Ok(AgentRootContinuationOutcome::NotReady);
        }
        if active_agent_count(&state) >= state.max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: state.max_concurrent_agents,
            });
        }
        self.install_root_terminal_router(&run_control)?;
        match run_control.begin_next_turn_after_success() {
            RunContinuationOutcome::Blocked => {
                return Ok(AgentRootContinuationOutcome::Blocked);
            }
            RunContinuationOutcome::Invalid => {
                return Ok(AgentRootContinuationOutcome::Invalid);
            }
            RunContinuationOutcome::Admitted => {}
        }

        let marker = Arc::new(());
        let root = state
            .agents
            .get_mut(&root_path)
            .expect("root existence was checked while holding the same registry lock");
        root.execution_marker = Some(Arc::clone(&marker));
        root.run_control = run_control.clone();
        drop(state);
        self.notify_activity();
        Ok(AgentRootContinuationOutcome::Admitted(
            AgentExecutionLease {
                control: self.clone(),
                path: root_path,
                marker,
                run_control,
            },
        ))
    }

    /// Applies the capacity selected for the next root run without replacing retained rows.
    /// Callers must only shrink the pool when the current active count fits the new limit.
    pub fn reconfigure_max_concurrent_agents(
        &self,
        max_concurrent_agents: usize,
    ) -> Result<(), AgentControlError> {
        if max_concurrent_agents == 0 {
            return Err(AgentControlError::InvalidCapacity);
        }
        let mut state = self.lock()?;
        if active_agent_count(&state) > max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents,
            });
        }
        state.max_concurrent_agents = max_concurrent_agents;
        drop(state);
        self.notify_activity();
        Ok(())
    }

    /// Restores a durable, inactive child row without consuming an execution slot.
    pub fn restore_inactive_child(
        &self,
        parent: &AgentPath,
        task_name: &str,
        session_id: SessionId,
        status: AgentStatus,
        initial_activity: Option<String>,
    ) -> Result<AgentSnapshot, AgentControlError> {
        let child_path = parent
            .join(task_name)
            .map_err(AgentControlError::InvalidPath)?;
        let mut state = self.lock()?;
        if !state.agents.contains_key(parent) {
            return Err(AgentControlError::AgentNotFound(parent.clone()));
        }
        if state.agents.contains_key(&child_path) {
            return Err(AgentControlError::AgentAlreadyExists(child_path));
        }
        if state
            .agents
            .values()
            .any(|agent| agent.session_id == session_id)
        {
            return Err(AgentControlError::SessionAlreadyRegistered(session_id));
        }

        let spawn_order = state.agents.len() as u64;
        let run_control = RunControl::new();
        let (mailbox_activity_tx, _) = watch::channel(0);
        state.agents.insert(
            child_path.clone(),
            AgentEntry {
                session_id,
                parent: Some(parent.clone()),
                spawn_order,
                status,
                last_activity: initial_activity,
                execution_marker: None,
                run_control,
                mailbox: VecDeque::new(),
                mailbox_generation: 0,
                trigger_admission_epoch: 0,
                trigger_purge_pending: 0,
                mailbox_activity_tx,
            },
        );
        let snapshot = snapshot_agent(&state, &child_path)
            .expect("a restored child must be available for its snapshot");
        drop(state);
        self.notify_activity();
        Ok(snapshot)
    }

    pub fn set_status(
        &self,
        path: &AgentPath,
        status: AgentStatus,
    ) -> Result<(), AgentControlError> {
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        agent.status = status;
        drop(state);
        self.notify_activity();
        Ok(())
    }

    pub fn set_activity(
        &self,
        path: &AgentPath,
        activity: Option<String>,
    ) -> Result<(), AgentControlError> {
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        agent.last_activity = activity;
        drop(state);
        self.notify_activity();
        Ok(())
    }

    pub fn status(&self, path: &AgentPath) -> Result<AgentStatus, AgentControlError> {
        let state = self.lock()?;
        Ok(state
            .agents
            .get(path)
            .map(|agent| agent.status.clone())
            .unwrap_or(AgentStatus::NotFound))
    }

    pub fn path_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<AgentPath>, AgentControlError> {
        let state = self.lock()?;
        Ok(state
            .agents
            .iter()
            .find_map(|(path, agent)| (agent.session_id == session_id).then(|| path.clone())))
    }

    pub fn list_agents(
        &self,
        prefix: Option<&AgentPath>,
    ) -> Result<Vec<AgentSnapshot>, AgentControlError> {
        let state = self.lock()?;
        let mut agents = state
            .agents
            .keys()
            .filter(|path| prefix.is_none_or(|prefix| path.is_at_or_below(prefix)))
            .filter_map(|path| snapshot_agent(&state, path))
            .collect::<Vec<_>>();
        agents.sort_by_key(|agent| agent.spawn_order);
        Ok(agents)
    }

    pub fn snapshot(&self) -> Result<AgentTreeSnapshot, AgentControlError> {
        let state = self.lock()?;
        let mut agents = state
            .agents
            .keys()
            .filter_map(|path| snapshot_agent(&state, path))
            .collect::<Vec<_>>();
        agents.sort_by_key(|agent| agent.spawn_order);
        let active_agent_count = agents.iter().filter(|agent| agent.is_active).count();
        Ok(AgentTreeSnapshot {
            root: AgentPath::root(),
            max_concurrent_agents: state.max_concurrent_agents,
            active_agent_count,
            agents,
        })
    }

    pub fn enqueue_mail(&self, message: AgentMailboxMessage) -> Result<u64, AgentControlError> {
        let recipient = message.recipient.clone();
        match self.enqueue_mail_after_durable_commit(message, false, || Ok(()))? {
            AgentMailDeliveryOutcome::Enqueued { generation, .. } => Ok(generation),
            AgentMailDeliveryOutcome::Suppressed => {
                Err(AgentControlError::MailboxClosed(recipient))
            }
        }
    }

    pub fn enqueue_mail_after_durable_commit(
        &self,
        message: AgentMailboxMessage,
        schedule_triggered: bool,
        durable_commit: impl FnOnce() -> Result<(), String>,
    ) -> Result<AgentMailDeliveryOutcome, AgentControlError> {
        let _delivery = self.lock_mail_delivery()?;
        let (author_session_id, recipient_session_id, trigger_admission_epoch) = {
            let state = self.lock()?;
            if schedule_triggered && self.inner.tree_control.is_cancelled() {
                return Err(AgentControlError::TreeCancelled);
            }
            let author = state
                .agents
                .get(&message.author)
                .ok_or_else(|| AgentControlError::AgentNotFound(message.author.clone()))?;
            let recipient = state
                .agents
                .get(&message.recipient)
                .ok_or_else(|| AgentControlError::AgentNotFound(message.recipient.clone()))?;
            if schedule_triggered && recipient.trigger_purge_pending > 0 {
                return Err(AgentControlError::MailboxClosed(message.recipient.clone()));
            }
            (
                author.session_id,
                recipient.session_id,
                recipient.trigger_admission_epoch,
            )
        };
        durable_commit().map_err(AgentControlError::DurableMailboxCommit)?;
        let mut state = self.lock()?;
        if !state
            .agents
            .get(&message.author)
            .is_some_and(|author| author.session_id == author_session_id)
        {
            return Err(AgentControlError::AgentNotFound(message.author.clone()));
        }
        let suppress_trigger = schedule_triggered
            && (self.inner.tree_control.is_cancelled()
                || !state
                    .agents
                    .get(&message.recipient)
                    .is_some_and(|recipient| {
                        recipient.session_id == recipient_session_id
                            && recipient.trigger_admission_epoch == trigger_admission_epoch
                            && recipient.trigger_purge_pending == 0
                            && !matches!(recipient.status, AgentStatus::Shutdown)
                    }));
        let recipient = state
            .agents
            .get_mut(&message.recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(message.recipient.clone()))?;
        if recipient.session_id != recipient_session_id {
            return Err(AgentControlError::AgentNotFound(message.recipient.clone()));
        }
        if suppress_trigger {
            return Ok(AgentMailDeliveryOutcome::Suppressed);
        }
        recipient.mailbox.push_back(message);
        recipient.mailbox_generation = recipient.mailbox_generation.wrapping_add(1);
        let generation = recipient.mailbox_generation;
        recipient.mailbox_activity_tx.send_replace(generation);
        let scheduled = if schedule_triggered {
            self.reserve_pending_triggered_executions_locked(&mut state)
        } else {
            Vec::new()
        };
        drop(state);
        self.notify_activity();
        Ok(AgentMailDeliveryOutcome::Enqueued {
            generation,
            scheduled,
        })
    }

    pub fn enqueue_mail_and_schedule(
        &self,
        message: AgentMailboxMessage,
    ) -> Result<(u64, Vec<AgentExecutionLease>), AgentControlError> {
        let recipient = message.recipient.clone();
        match self.enqueue_mail_after_durable_commit(message, true, || Ok(()))? {
            AgentMailDeliveryOutcome::Enqueued {
                generation,
                scheduled,
            } => Ok((generation, scheduled)),
            AgentMailDeliveryOutcome::Suppressed => {
                Err(AgentControlError::MailboxClosed(recipient))
            }
        }
    }

    pub fn complete_execution(
        &self,
        lease: AgentExecutionLease,
        status: AgentStatus,
        activity: Option<String>,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(&lease.path)
            .ok_or_else(|| AgentControlError::AgentNotFound(lease.path.clone()))?;
        if !agent
            .execution_marker
            .as_ref()
            .is_some_and(|marker| Arc::ptr_eq(marker, &lease.marker))
        {
            return Err(AgentControlError::StaleExecution(lease.path.clone()));
        }
        agent.status = status;
        agent.last_activity = activity;
        agent.execution_marker = None;
        let scheduled = if self.inner.tree_control.is_cancelled() {
            Vec::new()
        } else {
            self.reserve_pending_triggered_executions_locked(&mut state)
        };
        drop(state);
        self.notify_activity();
        drop(lease);
        Ok(scheduled)
    }

    pub fn remove_agent(&self, path: &AgentPath) -> Result<(), AgentControlError> {
        if path.is_root() {
            return Err(AgentControlError::AgentHasChildren(path.clone()));
        }
        let _delivery = self.lock_mail_delivery()?;
        let mut state = self.lock()?;
        if state
            .agents
            .values()
            .any(|agent| agent.parent.as_ref() == Some(path))
        {
            return Err(AgentControlError::AgentHasChildren(path.clone()));
        }
        let agent = state
            .agents
            .remove(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        agent.run_control.supersede();
        drop(state);
        self.notify_activity();
        Ok(())
    }

    pub fn is_quiescent(&self) -> Result<bool, AgentControlError> {
        let state = self.lock()?;
        let no_active = state
            .agents
            .values()
            .all(|agent| agent.execution_marker.is_none());
        Ok(no_active
            && (self.inner.tree_control.is_cancelled()
                || state
                    .agents
                    .values()
                    .all(|agent| !agent.mailbox.iter().any(|message| message.trigger_turn))))
    }

    pub fn activity_generation(&self) -> u64 {
        *self.inner.activity_tx.borrow()
    }

    pub async fn wait_for_activity(
        &self,
        observed_generation: u64,
    ) -> Result<u64, AgentControlError> {
        let mut activity = self.inner.activity_tx.subscribe();
        let current = *activity.borrow_and_update();
        if current != observed_generation {
            return Ok(current);
        }
        activity
            .changed()
            .await
            .map_err(|_| AgentControlError::MailboxClosed(AgentPath::root()))?;
        Ok(*activity.borrow_and_update())
    }

    pub fn drain_mailbox(
        &self,
        recipient: &AgentPath,
    ) -> Result<Vec<AgentMailboxMessage>, AgentControlError> {
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient.clone()))?;
        let messages = agent.mailbox.drain(..).collect();
        drop(state);
        self.notify_activity();
        Ok(messages)
    }

    pub fn mailbox_senders(
        &self,
        recipient: &AgentPath,
    ) -> Result<Vec<AgentPath>, AgentControlError> {
        let state = self.lock()?;
        let agent = state
            .agents
            .get(recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient.clone()))?;
        let mut senders = Vec::new();
        for message in &agent.mailbox {
            if !senders.contains(&message.author) {
                senders.push(message.author.clone());
            }
        }
        Ok(senders)
    }

    pub fn mailbox_has_trigger_turn(
        &self,
        recipient: &AgentPath,
    ) -> Result<bool, AgentControlError> {
        let state = self.lock()?;
        let agent = state
            .agents
            .get(recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient.clone()))?;
        Ok(agent.mailbox.iter().any(|message| message.trigger_turn))
    }

    pub fn subscribe_mailbox(
        &self,
        recipient: &AgentPath,
    ) -> Result<watch::Receiver<u64>, AgentControlError> {
        let state = self.lock()?;
        let agent = state
            .agents
            .get(recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient.clone()))?;
        Ok(agent.mailbox_activity_tx.subscribe())
    }

    pub async fn wait_for_mailbox_activity(
        &self,
        recipient: &AgentPath,
        observed_generation: u64,
    ) -> Result<u64, AgentControlError> {
        let mut activity = self.subscribe_mailbox(recipient)?;
        let current_generation = *activity.borrow_and_update();
        if current_generation != observed_generation {
            return Ok(current_generation);
        }
        activity
            .changed()
            .await
            .map_err(|_| AgentControlError::MailboxClosed(recipient.clone()))?;
        Ok(*activity.borrow_and_update())
    }

    pub fn cancel_agent(&self, path: &AgentPath) -> Result<(), AgentControlError> {
        let run_control = {
            let state = self.lock()?;
            let agent = state
                .agents
                .get(path)
                .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
            if agent.execution_marker.is_none() {
                return Err(AgentControlError::AgentNotActive(path.clone()));
            }
            agent.run_control.clone()
        };
        run_control.interrupt(TurnInterruptionCause::AgentInterrupted);
        self.notify_activity();
        Ok(())
    }

    /// Stops work whose durable session was terminalized outside the current worker.
    /// Unlike `cancel_agent`, a child cannot restart from an already queued trigger turn.
    pub fn cancel_for_durable_terminal(&self, path: &AgentPath) -> Result<(), AgentControlError> {
        if path.is_root() {
            self.supersede_tree();
            return Ok(());
        }

        let (terminal_session_id, terminal_epoch) = {
            let mut state = self.lock()?;
            let agent = state
                .agents
                .get_mut(path)
                .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
            agent.run_control.supersede();
            if agent.trigger_purge_pending == 0 {
                agent.trigger_admission_epoch = agent.trigger_admission_epoch.wrapping_add(1);
            }
            agent.trigger_purge_pending = agent.trigger_purge_pending.saturating_add(1);
            (agent.session_id, agent.trigger_admission_epoch)
        };
        self.notify_activity();

        let _delivery = self.lock_mail_delivery()?;
        let mut state = self.lock()?;
        let Some(agent) = state.agents.get_mut(path) else {
            return Ok(());
        };
        if agent.session_id != terminal_session_id
            || agent.trigger_admission_epoch != terminal_epoch
        {
            return Ok(());
        }
        let pending_before = agent.mailbox.len();
        agent.mailbox.retain(|message| !message.trigger_turn);
        agent.trigger_purge_pending = agent.trigger_purge_pending.saturating_sub(1);
        if agent.mailbox.len() != pending_before {
            agent.mailbox_generation = agent.mailbox_generation.wrapping_add(1);
            agent
                .mailbox_activity_tx
                .send_replace(agent.mailbox_generation);
        }
        drop(state);
        self.notify_activity();
        Ok(())
    }

    pub fn interrupt_tree(&self, root_cause: TurnInterruptionCause) -> bool {
        self.cancel_tree_with_root_cause(root_cause)
    }

    /// Classifies a permission Abort at the root and its requesting agent as one action.
    ///
    /// The requesting agent receives `ApprovalAborted`, while all other descendants receive
    /// `TreeStopped`. If a Stop, failure, or supersession already owns either the root or the
    /// requesting run, the Abort is rejected without classifying the other owner. A detached child
    /// may still abort after root success is sealed; the sealed root is preserved while the tree
    /// owner and requester are classified together.
    pub fn abort_tree_from_permission(
        &self,
        requesting_control: &RunControl,
    ) -> crate::runtime::RunCancelOutcome {
        let Ok(state) = self.lock() else {
            return crate::runtime::RunCancelOutcome::Rejected;
        };
        let Some(root) = state
            .agents
            .iter()
            .find_map(|(path, agent)| path.is_root().then_some(agent))
        else {
            return crate::runtime::RunCancelOutcome::Rejected;
        };
        let requesting_path = state.agents.iter().find_map(|(path, agent)| {
            agent
                .run_control
                .same_owner(requesting_control)
                .then_some(path)
        });
        let Some(requesting_path) = requesting_path else {
            return crate::runtime::RunCancelOutcome::Rejected;
        };

        let approval_aborted = crate::runtime::RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted,
        );
        let detached_after_root_success = root.run_control.success_is_sealed();
        let outcome = if detached_after_root_success {
            root.run_control.block_continuation_local();
            let requester_is_live_descendant = !requesting_path.is_root()
                && state
                    .agents
                    .get(requesting_path)
                    .is_some_and(agent_has_live_work);
            if !requester_is_live_descendant {
                return crate::runtime::RunCancelOutcome::Rejected;
            }
            RunControl::request_linked_cancellation(
                &self.inner.tree_control,
                crate::runtime::RunCancellationCause::Interruption(
                    TurnInterruptionCause::TreeStopped,
                ),
                requesting_control,
                approval_aborted,
            )
        } else {
            RunControl::request_linked_cancellation(
                &root.run_control,
                approval_aborted.clone(),
                requesting_control,
                approval_aborted,
            )
        };
        if outcome == crate::runtime::RunCancelOutcome::Rejected {
            return outcome;
        }

        let tree_stopped =
            crate::runtime::RunCancellationCause::Interruption(TurnInterruptionCause::TreeStopped);
        // `tree_control` is private and every active-root tree producer is serialized by `state`,
        // so a root/requester linked claim cannot race a different tree classification here. The
        // detached sealed-root branch already linked `tree_control` with the requester above.
        let tree_owns_stop = if detached_after_root_success {
            true
        } else {
            let tree_outcome = self.inner.tree_control.request_cancel(tree_stopped.clone());
            tree_outcome != crate::runtime::RunCancelOutcome::Rejected
                || self.inner.tree_control.cause().as_ref() == Some(&tree_stopped)
        };
        debug_assert!(
            tree_owns_stop,
            "an accepted root permission Abort must own the tree terminal classification"
        );
        if tree_owns_stop {
            for (path, agent) in &state.agents {
                if !path.is_root() && !agent.run_control.same_owner(requesting_control) {
                    agent.run_control.request_cancel(tree_stopped.clone());
                }
            }
        }
        drop(state);
        self.notify_activity();
        outcome
    }

    fn supersede_tree(&self) -> bool {
        self.classify_tree(
            crate::runtime::RunCancellationCause::Superseded,
            crate::runtime::RunCancellationCause::Superseded,
            false,
        )
    }

    pub fn fail_tree(&self, message: impl Into<String>) -> bool {
        self.classify_failure_tree(message).changed()
    }

    fn route_terminal_outcome(
        &self,
        source: &RunControl,
        kind: RunTerminalRouteKind,
        cause: crate::runtime::RunCancellationCause,
    ) -> Option<RunCancelOutcome> {
        let (descendant_cause, allow_detached_tree_action) = routed_descendant_cause(&cause);
        let result = self.classify_tree_result(
            cause,
            descendant_cause,
            allow_detached_tree_action,
            Some((source, kind)),
        );
        result.source_matched.then_some(result.root_outcome)
    }

    fn classify_failure_tree(&self, message: impl Into<String>) -> TreeClassificationResult {
        let message = message.into();
        self.classify_tree_result(
            crate::runtime::RunCancellationCause::Failure(message.clone()),
            crate::runtime::RunCancellationCause::Failure(message),
            false,
            None,
        )
    }

    /// Reconciles an exact-turn durable root terminal after another in-memory producer may have
    /// won locally before that durable commit was observed.
    ///
    /// The root and each descendant retain their first locally visible classification, while the
    /// durable terminal still closes the tree-wide scheduling owner and reaches every descendant.
    pub fn reconcile_durable_root_terminal(
        &self,
        root_cause: crate::runtime::RunCancellationCause,
    ) -> bool {
        let descendant_cause = match &root_cause {
            crate::runtime::RunCancellationCause::Interruption(_) => {
                crate::runtime::RunCancellationCause::Interruption(
                    TurnInterruptionCause::TreeStopped,
                )
            }
            crate::runtime::RunCancellationCause::Superseded => {
                crate::runtime::RunCancellationCause::Superseded
            }
            crate::runtime::RunCancellationCause::Failure(message) => {
                crate::runtime::RunCancellationCause::Failure(message.clone())
            }
        };
        let Ok(state) = self.lock() else {
            return false;
        };
        let Some(root) = state
            .agents
            .iter()
            .find_map(|(path, agent)| path.is_root().then_some(agent))
        else {
            return false;
        };
        let root_applied = matches!(
            root.run_control.request_cancel_local(root_cause),
            crate::runtime::RunCancelOutcome::Applied
        );
        let tree_applied = self.inner.tree_control.cancel(descendant_cause.clone());
        if !tree_applied && !self.inner.tree_control.is_cancelled() {
            return root_applied;
        }
        for (path, agent) in &state.agents {
            if !path.is_root() {
                agent.run_control.cancel(descendant_cause.clone());
            }
        }
        drop(state);
        self.notify_activity();
        root_applied || tree_applied
    }

    fn cancel_tree_with_root_cause(&self, root_cause: TurnInterruptionCause) -> bool {
        self.classify_tree(
            crate::runtime::RunCancellationCause::Interruption(root_cause),
            crate::runtime::RunCancellationCause::Interruption(TurnInterruptionCause::TreeStopped),
            true,
        )
    }

    fn classify_tree(
        &self,
        root_cause: crate::runtime::RunCancellationCause,
        descendant_cause: crate::runtime::RunCancellationCause,
        allow_detached_tree_action: bool,
    ) -> bool {
        self.classify_tree_result(
            root_cause,
            descendant_cause,
            allow_detached_tree_action,
            None,
        )
        .changed()
    }

    fn classify_tree_result(
        &self,
        root_cause: crate::runtime::RunCancellationCause,
        descendant_cause: crate::runtime::RunCancellationCause,
        allow_detached_tree_action: bool,
        root_route: Option<(&RunControl, RunTerminalRouteKind)>,
    ) -> TreeClassificationResult {
        let Ok(state) = self.lock() else {
            return if root_route.is_some() {
                TreeClassificationResult::unroutable()
            } else {
                TreeClassificationResult::rejected()
            };
        };
        let Some(root) = state
            .agents
            .iter()
            .find_map(|(path, agent)| path.is_root().then_some(agent))
        else {
            return if root_route.is_some() {
                TreeClassificationResult::unroutable()
            } else {
                TreeClassificationResult::rejected()
            };
        };
        if root_route.is_some_and(|(source, _)| !root.run_control.same_owner(source)) {
            return TreeClassificationResult::unroutable();
        }
        let mut effective_root_cause = root_cause;
        let mut effective_descendant_cause = descendant_cause;
        let mut effective_allow_detached_tree_action = allow_detached_tree_action;
        let preclassified_root_outcome = match root_route.map(|(_, kind)| kind) {
            Some(RunTerminalRouteKind::ResolveSuccessCommitAuthoritatively) => Some(
                if root
                    .run_control
                    .resolve_success_commit_authoritatively_local(effective_root_cause.clone())
                {
                    RunCancelOutcome::Applied
                } else {
                    RunCancelOutcome::Rejected
                },
            ),
            Some(RunTerminalRouteKind::AbandonSuccessCommit) => {
                match root
                    .run_control
                    .abandon_success_commit_local(effective_root_cause.clone())
                {
                    Some(actual_cause) => {
                        let (actual_descendant_cause, actual_allow_detached) =
                            routed_descendant_cause(&actual_cause);
                        effective_root_cause = actual_cause;
                        effective_descendant_cause = actual_descendant_cause;
                        effective_allow_detached_tree_action = actual_allow_detached;
                        Some(RunCancelOutcome::Applied)
                    }
                    None => Some(RunCancelOutcome::Rejected),
                }
            }
            Some(RunTerminalRouteKind::Request | RunTerminalRouteKind::ReleaseSuccessCommit)
            | None => None,
        };
        let root_success_is_durable = root.run_control.success_is_sealed();
        let detached_request =
            matches!(root_route, None | Some((_, RunTerminalRouteKind::Request)));
        if detached_request && root_success_is_durable && effective_allow_detached_tree_action {
            root.run_control.block_continuation_local();
            if root_route.is_none()
                && !state
                    .agents
                    .iter()
                    .any(|(path, agent)| !path.is_root() && agent_has_live_work(agent))
            {
                return TreeClassificationResult::rejected();
            }
            let tree_applied = self
                .inner
                .tree_control
                .cancel(effective_descendant_cause.clone());
            let tree_owns_requested_cause = tree_applied
                || self.inner.tree_control.cause().as_ref() == Some(&effective_descendant_cause);
            if !tree_owns_requested_cause {
                return TreeClassificationResult::rejected();
            }
            for (path, agent) in &state.agents {
                if !path.is_root() {
                    agent.run_control.cancel(effective_descendant_cause.clone());
                }
            }
            drop(state);
            self.notify_activity();
            return TreeClassificationResult {
                root_outcome: RunCancelOutcome::Rejected,
                tree_applied,
                source_matched: true,
            };
        }
        let root_outcome = preclassified_root_outcome.unwrap_or_else(|| {
            root.run_control
                .request_cancel_local(effective_root_cause.clone())
        });
        let root_owns_requested_cause =
            matches!(root_outcome, crate::runtime::RunCancelOutcome::Applied)
                || root.run_control.cause().as_ref() == Some(&effective_root_cause);
        let (deferred_tree_action, deferred_requires_live_descendant) = match root_outcome {
            crate::runtime::RunCancelOutcome::Deferred(deferral) => (
                effective_allow_detached_tree_action || !deferral.is_success_commit_only(),
                root_route.is_none() && deferral.is_success_commit_only(),
            ),
            crate::runtime::RunCancelOutcome::Applied
            | crate::runtime::RunCancelOutcome::Rejected => (false, false),
        };
        if !root_owns_requested_cause && !deferred_tree_action {
            return TreeClassificationResult {
                root_outcome,
                tree_applied: false,
                source_matched: true,
            };
        }
        if deferred_tree_action
            && deferred_requires_live_descendant
            && !state
                .agents
                .iter()
                .any(|(path, agent)| !path.is_root() && agent_has_live_work(agent))
        {
            return TreeClassificationResult {
                root_outcome,
                tree_applied: false,
                source_matched: true,
            };
        }
        let tree_applied = self
            .inner
            .tree_control
            .cancel(effective_descendant_cause.clone());
        let tree_owns_requested_cause = tree_applied
            || self.inner.tree_control.cause().as_ref() == Some(&effective_descendant_cause);
        if !tree_owns_requested_cause {
            return TreeClassificationResult {
                root_outcome,
                tree_applied: false,
                source_matched: true,
            };
        }
        for (path, agent) in &state.agents {
            if !path.is_root() {
                agent.run_control.cancel(effective_descendant_cause.clone());
            }
        }
        drop(state);
        self.notify_activity();
        TreeClassificationResult {
            root_outcome,
            tree_applied,
            source_matched: true,
        }
    }

    pub fn tree_is_cancelled(&self) -> bool {
        self.inner.tree_control.is_cancelled()
    }

    fn release_execution(&self, path: &AgentPath, marker: &Arc<()>) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        let Some(agent) = state.agents.get_mut(path) else {
            return;
        };
        if agent
            .execution_marker
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, marker))
        {
            agent.execution_marker = None;
            drop(state);
            self.notify_activity();
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, AgentTreeState>, AgentControlError> {
        self.inner
            .state
            .lock()
            .map_err(|_| AgentControlError::LockPoisoned)
    }

    fn lock_mail_delivery(&self) -> Result<MutexGuard<'_, ()>, AgentControlError> {
        self.inner
            .mail_delivery
            .lock()
            .map_err(|_| AgentControlError::LockPoisoned)
    }

    fn reserve_pending_triggered_executions_locked(
        &self,
        state: &mut AgentTreeState,
    ) -> Vec<AgentExecutionLease> {
        let mut candidates = state
            .agents
            .iter()
            .filter_map(|(path, agent)| {
                (!path.is_root()
                    && agent.execution_marker.is_none()
                    && !matches!(agent.status, AgentStatus::Shutdown)
                    && agent.mailbox.iter().any(|message| message.trigger_turn))
                .then_some((agent.spawn_order, path.clone()))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(spawn_order, _)| *spawn_order);
        let mut leases = Vec::new();
        for (_, path) in candidates {
            if active_agent_count(state) >= state.max_concurrent_agents {
                break;
            }
            let marker = Arc::new(());
            let run_control = RunControl::new();
            let agent = state
                .agents
                .get_mut(&path)
                .expect("scheduled agent was selected from this registry");
            agent.execution_marker = Some(marker.clone());
            agent.run_control = run_control.clone();
            agent.status = AgentStatus::PendingInit;
            leases.push(AgentExecutionLease {
                control: self.clone(),
                path,
                marker,
                run_control,
            });
        }
        leases
    }

    fn notify_activity(&self) {
        self.inner
            .activity_tx
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    fn install_root_terminal_router(
        &self,
        run_control: &RunControl,
    ) -> Result<(), AgentControlError> {
        run_control
            .install_terminal_router(&self.inner.root_terminal_router)
            .map_err(|()| AgentControlError::RunControlOwnedByDifferentTree)
    }
}

impl AgentExecutionLease {
    pub fn path(&self) -> &AgentPath {
        &self.path
    }

    pub fn run_control(&self) -> RunControl {
        self.run_control.clone()
    }

    pub fn cancel_token(&self) -> tokio_util::sync::CancellationToken {
        self.run_control.token()
    }
}

impl Drop for AgentExecutionLease {
    fn drop(&mut self) {
        self.control.release_execution(&self.path, &self.marker);
    }
}

fn active_agent_count(state: &AgentTreeState) -> usize {
    state
        .agents
        .values()
        .filter(|agent| agent.execution_marker.is_some())
        .count()
}

fn agent_has_live_work(agent: &AgentEntry) -> bool {
    agent.execution_marker.is_some() || agent.mailbox.iter().any(|message| message.trigger_turn)
}

fn routed_descendant_cause(
    root_cause: &crate::runtime::RunCancellationCause,
) -> (crate::runtime::RunCancellationCause, bool) {
    match root_cause {
        crate::runtime::RunCancellationCause::Interruption(_) => (
            crate::runtime::RunCancellationCause::Interruption(TurnInterruptionCause::TreeStopped),
            true,
        ),
        crate::runtime::RunCancellationCause::Superseded => {
            (crate::runtime::RunCancellationCause::Superseded, false)
        }
        crate::runtime::RunCancellationCause::Failure(message) => (
            crate::runtime::RunCancellationCause::Failure(message.clone()),
            false,
        ),
    }
}

fn snapshot_agent(state: &AgentTreeState, path: &AgentPath) -> Option<AgentSnapshot> {
    let agent = state.agents.get(path)?;
    let mut children = state
        .agents
        .iter()
        .filter_map(|(child_path, child)| {
            (child.parent.as_ref() == Some(path)).then(|| (child.spawn_order, child_path.clone()))
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|(spawn_order, _)| *spawn_order);
    Some(AgentSnapshot {
        path: path.clone(),
        session_id: agent.session_id,
        parent: agent.parent.clone(),
        children: children.into_iter().map(|(_, path)| path).collect(),
        spawn_order: agent.spawn_order,
        status: agent.status.clone(),
        last_activity: agent.last_activity.clone(),
        is_active: agent.execution_marker.is_some(),
        mailbox_generation: agent.mailbox_generation,
        pending_mail_count: agent.mailbox.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RunCancellationCause;

    #[derive(Clone, Copy, Debug)]
    enum TreeTerminalProducer {
        UserStop,
        ApprovalAbort,
        Failure,
        Superseded,
    }

    fn apply_tree_terminal_producer(
        control: &AgentControl,
        producer: TreeTerminalProducer,
    ) -> bool {
        match producer {
            TreeTerminalProducer::UserStop => {
                control.interrupt_tree(TurnInterruptionCause::UserStop)
            }
            TreeTerminalProducer::ApprovalAbort => {
                control.interrupt_tree(TurnInterruptionCause::ApprovalAborted)
            }
            TreeTerminalProducer::Failure => control.fail_tree("operational failure"),
            TreeTerminalProducer::Superseded => {
                control
                    .cancel_for_durable_terminal(&AgentPath::root())
                    .expect("supersede tree");
                false
            }
        }
    }

    #[test]
    fn active_root_success_commit_preserves_root_success_but_explicit_actions_stop_children() {
        for producer in [
            TreeTerminalProducer::UserStop,
            TreeTerminalProducer::ApprovalAbort,
        ] {
            let root_control = RunControl::new();
            let (control, root_execution) =
                AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                    .expect("agent tree");
            let (_, child_execution) = control
                .register_child(&AgentPath::root(), "child", SessionId::new(), None)
                .expect("child");
            let child_control = child_execution.run_control();
            let success_commit = root_control
                .begin_success_commit()
                .expect("success reservation");

            assert!(apply_tree_terminal_producer(&control, producer));
            assert_eq!(
                child_control.cause(),
                Some(RunCancellationCause::Interruption(
                    TurnInterruptionCause::TreeStopped
                )),
                "producer={producer:?}"
            );
            assert!(control.tree_is_cancelled(), "producer={producer:?}");
            assert_eq!(root_control.cause(), None, "producer={producer:?}");
            assert!(success_commit.seal());
            assert_eq!(root_control.cause(), None, "producer={producer:?}");

            control
                .complete_execution(root_execution, AgentStatus::Completed(None), None)
                .expect("complete root");
            control
                .complete_execution(child_execution, AgentStatus::Interrupted, None)
                .expect("complete child");
        }
    }

    #[test]
    fn raw_current_root_interrupt_routes_cli_stop_to_the_whole_tree() {
        let root_control = RunControl::new();
        let (control, _root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();

        assert!(root_control.interrupt(TurnInterruptionCause::UserStop));

        assert_eq!(
            root_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        assert!(control.tree_is_cancelled());
        assert!(child_control.begin_tool_effect_admission().is_none());
    }

    #[test]
    fn raw_current_root_interrupt_preserves_sealed_success_and_stops_descendants() {
        let root_control = RunControl::new();
        let (control, _root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        assert!(root_control.seal_success());

        assert_eq!(
            root_control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
            RunCancelOutcome::Rejected
        );

        assert!(root_control.success_is_sealed());
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        assert!(control.tree_is_cancelled());
    }

    #[derive(Clone, Copy, Debug)]
    enum RootSuccessStopCase {
        Committing,
        Sealed,
    }

    #[test]
    fn raw_explicit_stop_latches_tree_without_descendants_during_or_after_success() {
        for success_case in [RootSuccessStopCase::Committing, RootSuccessStopCase::Sealed] {
            let root_control = RunControl::new();
            let (control, root_execution) =
                AgentControl::with_root_control(SessionId::new(), 1, root_control.clone())
                    .expect("agent tree");

            match success_case {
                RootSuccessStopCase::Committing => {
                    let success = root_control.begin_success_commit().expect("success commit");
                    assert!(matches!(
                        root_control.request_cancel(RunCancellationCause::Interruption(
                            TurnInterruptionCause::UserStop,
                        )),
                        RunCancelOutcome::Deferred(_)
                    ));
                    assert!(control.tree_is_cancelled());
                    assert!(success.seal());
                }
                RootSuccessStopCase::Sealed => {
                    assert!(root_control.seal_success());
                    assert_eq!(
                        root_control.request_cancel(RunCancellationCause::Interruption(
                            TurnInterruptionCause::UserStop,
                        )),
                        RunCancelOutcome::Rejected
                    );
                }
            }

            assert!(root_control.success_is_sealed());
            assert!(control.tree_is_cancelled());
            control
                .complete_execution(root_execution, AgentStatus::Completed(None), None)
                .expect("complete root");
            assert!(matches!(
                control.try_acquire_execution(&AgentPath::root()),
                Err(AgentControlError::TreeCancelled)
            ));
        }
    }

    #[test]
    fn explicit_stop_before_root_continuation_claim_preserves_success_and_blocks_admission() {
        let root_control = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 1, root_control.clone())
                .expect("agent tree");
        let success = root_control.begin_success_commit().expect("success commit");

        assert!(matches!(
            root_control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
            RunCancelOutcome::Deferred(_)
        ));
        assert!(success.seal());
        control
            .complete_execution(root_execution, AgentStatus::Completed(None), None)
            .expect("complete root");

        assert!(matches!(
            control
                .try_acquire_root_continuation(root_control.clone())
                .expect("continuation outcome"),
            AgentRootContinuationOutcome::Blocked
        ));
        assert!(root_control.success_is_sealed());
        assert_eq!(root_control.cause(), None);
        assert!(control.tree_is_cancelled());
    }

    #[test]
    fn root_continuation_claim_before_stop_keeps_the_same_tree_owner() {
        let root_control = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 1, root_control.clone())
                .expect("agent tree");
        assert!(root_control.seal_success());
        control
            .complete_execution(root_execution, AgentStatus::Completed(None), None)
            .expect("complete root");

        let continuation = match control
            .try_acquire_root_continuation(root_control.clone())
            .expect("continuation outcome")
        {
            AgentRootContinuationOutcome::Admitted(lease) => lease,
            AgentRootContinuationOutcome::Blocked
            | AgentRootContinuationOutcome::NotReady
            | AgentRootContinuationOutcome::Invalid => panic!("continuation was not admitted"),
        };
        assert!(!root_control.success_is_sealed());

        assert_eq!(
            root_control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
            RunCancelOutcome::Applied
        );
        assert_eq!(
            root_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert!(continuation.cancel_token().is_cancelled());
        assert!(control.tree_is_cancelled());
        control
            .complete_execution(continuation, AgentStatus::Interrupted, None)
            .expect("complete cancelled continuation");
    }

    #[test]
    fn root_continuation_waits_for_pending_trigger_work_under_the_registry_lock() {
        let root_control = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (child, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        assert!(matches!(
            control
                .try_acquire_root_continuation(root_control.clone())
                .expect("active tree continuation outcome"),
            AgentRootContinuationOutcome::NotReady
        ));
        control
            .complete_execution(child_execution, AgentStatus::Completed(None), None)
            .expect("complete child");
        control
            .enqueue_mail(AgentMailboxMessage::new(
                AgentPath::root(),
                child.path.clone(),
                "follow-up",
                true,
            ))
            .expect("pending trigger mail");
        assert!(root_control.seal_success());
        control
            .complete_execution(root_execution, AgentStatus::Completed(None), None)
            .expect("complete root");

        assert!(matches!(
            control
                .try_acquire_root_continuation(root_control.clone())
                .expect("continuation outcome"),
            AgentRootContinuationOutcome::NotReady
        ));
        control
            .drain_mailbox(&child.path)
            .expect("drain trigger mail");
        let continuation = match control
            .try_acquire_root_continuation(root_control.clone())
            .expect("continuation outcome")
        {
            AgentRootContinuationOutcome::Admitted(lease) => lease,
            AgentRootContinuationOutcome::Blocked
            | AgentRootContinuationOutcome::NotReady
            | AgentRootContinuationOutcome::Invalid => panic!("continuation was not admitted"),
        };
        drop(continuation);
    }

    #[test]
    fn raw_root_supersession_routes_open_and_deferred_release_to_the_whole_tree() {
        for deferred in [false, true] {
            let root_control = RunControl::new();
            let (control, _root_execution) =
                AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                    .expect("agent tree");
            let (_, child_execution) = control
                .register_child(&AgentPath::root(), "child", SessionId::new(), None)
                .expect("child");
            let child_control = child_execution.run_control();
            let success =
                deferred.then(|| root_control.begin_success_commit().expect("success commit"));

            let outcome = root_control.request_cancel(RunCancellationCause::Superseded);
            if let Some(success) = success {
                assert_eq!(
                    outcome,
                    RunCancelOutcome::Deferred(crate::runtime::RunCancelDeferral {
                        primary: crate::runtime::RunReservationKind::SuccessCommit,
                        secondary: None,
                    })
                );
                assert_eq!(root_control.cause(), None);
                assert_eq!(child_control.cause(), None);
                assert!(!control.tree_is_cancelled());
                success.release();
            } else {
                assert_eq!(outcome, RunCancelOutcome::Applied);
            }

            assert_eq!(root_control.cause(), Some(RunCancellationCause::Superseded));
            assert_eq!(
                child_control.cause(),
                Some(RunCancellationCause::Superseded)
            );
            assert!(control.tree_is_cancelled());
            assert!(child_control.begin_tool_effect_admission().is_none());
        }
    }

    #[test]
    fn active_root_success_commit_blocks_internal_tree_terminal_producers() {
        for producer in [
            TreeTerminalProducer::Failure,
            TreeTerminalProducer::Superseded,
        ] {
            let root_control = RunControl::new();
            let (control, root_execution) =
                AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                    .expect("agent tree");
            let (_, child_execution) = control
                .register_child(&AgentPath::root(), "child", SessionId::new(), None)
                .expect("child");
            let child_control = child_execution.run_control();
            let success_commit = root_control
                .begin_success_commit()
                .expect("success reservation");

            assert!(!apply_tree_terminal_producer(&control, producer));
            assert_eq!(child_control.cause(), None, "producer={producer:?}");
            assert!(!control.tree_is_cancelled(), "producer={producer:?}");
            assert!(success_commit.seal());
            assert_eq!(root_control.cause(), None, "producer={producer:?}");

            control
                .complete_execution(root_execution, AgentStatus::Completed(None), None)
                .expect("complete root");
            control
                .complete_execution(child_execution, AgentStatus::Completed(None), None)
                .expect("complete child");
        }
    }

    #[test]
    fn root_failure_deferred_by_tool_settlement_stops_descendants_before_commit_releases() {
        let root_control = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        let settlement = root_control
            .begin_tool_settlement()
            .expect("root tool settlement");
        let failure = RunCancellationCause::Failure("durable tool commit failed".to_string());

        assert!(control.fail_tree("durable tool commit failed"));
        assert_eq!(root_control.cause(), None);
        assert_eq!(child_control.cause(), Some(failure.clone()));
        assert!(child_control.begin_tool_effect_admission().is_none());
        assert!(control.tree_is_cancelled());

        settlement.release();
        assert_eq!(root_control.cause(), Some(failure));

        control
            .complete_execution(
                root_execution,
                AgentStatus::Errored("failed".to_string()),
                None,
            )
            .expect("complete root");
        control
            .complete_execution(
                child_execution,
                AgentStatus::Errored("tree failed".to_string()),
                None,
            )
            .expect("complete child");
    }

    #[derive(Clone, Copy, Debug)]
    enum RootFailureReservationCase {
        EffectAdmission,
        EffectCommit,
        ToolSettlement,
    }

    #[test]
    fn root_terminal_router_rejects_sibling_effects_before_deferred_root_release() {
        for reservation_case in [
            RootFailureReservationCase::EffectAdmission,
            RootFailureReservationCase::EffectCommit,
            RootFailureReservationCase::ToolSettlement,
        ] {
            let root_control = RunControl::new();
            let (control, root_execution) =
                AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                    .expect("agent tree");
            let (_, child_execution) = control
                .register_child(&AgentPath::root(), "child", SessionId::new(), None)
                .expect("child");
            let child_control = child_execution.run_control();
            let failure = RunCancellationCause::Failure(format!(
                "heartbeat failed during {reservation_case:?}"
            ));
            let (expected_kind, release): (crate::runtime::RunReservationKind, Box<dyn FnOnce()>) =
                match reservation_case {
                    RootFailureReservationCase::EffectAdmission => {
                        let reservation = root_control
                            .begin_tool_effect_admission()
                            .expect("effect admission");
                        let expected_failure = failure.clone();
                        (
                            crate::runtime::RunReservationKind::ToolEffectAdmission,
                            Box::new(move || {
                                assert_eq!(reservation.admit(), Err(expected_failure));
                            }),
                        )
                    }
                    RootFailureReservationCase::EffectCommit => {
                        let reservation = root_control
                            .begin_tool_effect_commit()
                            .expect("effect commit");
                        (
                            crate::runtime::RunReservationKind::ToolEffectCommit,
                            Box::new(move || reservation.release()),
                        )
                    }
                    RootFailureReservationCase::ToolSettlement => {
                        let reservation = root_control
                            .begin_tool_settlement()
                            .expect("tool settlement");
                        (
                            crate::runtime::RunReservationKind::ToolSettlement,
                            Box::new(move || reservation.release()),
                        )
                    }
                };

            assert_eq!(
                root_control.request_cancel(failure.clone()),
                RunCancelOutcome::Deferred(crate::runtime::RunCancelDeferral {
                    primary: expected_kind,
                    secondary: None,
                }),
                "reservation={reservation_case:?}"
            );
            assert_eq!(
                root_control.cause(),
                None,
                "reservation={reservation_case:?}"
            );
            assert_eq!(
                child_control.cause(),
                Some(failure.clone()),
                "reservation={reservation_case:?}"
            );
            assert!(
                control.tree_is_cancelled(),
                "reservation={reservation_case:?}"
            );
            assert!(
                child_control.begin_tool_effect_admission().is_none(),
                "reservation={reservation_case:?}"
            );

            release();
            assert_eq!(
                root_control.cause(),
                Some(failure),
                "reservation={reservation_case:?}"
            );
            control
                .complete_execution(
                    root_execution,
                    AgentStatus::Errored("root failed".to_string()),
                    None,
                )
                .expect("complete root");
            control
                .complete_execution(
                    child_execution,
                    AgentStatus::Errored("tree failed".to_string()),
                    None,
                )
                .expect("complete child");
        }
    }

    #[test]
    fn root_terminal_router_preserves_success_commit_and_sealed_success() {
        let root_control = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        let success = root_control.begin_success_commit().expect("success commit");

        assert_eq!(
            root_control.request_cancel(RunCancellationCause::Failure(
                "late operational failure".to_string()
            )),
            RunCancelOutcome::Deferred(crate::runtime::RunCancelDeferral {
                primary: crate::runtime::RunReservationKind::SuccessCommit,
                secondary: None,
            })
        );
        assert_eq!(child_control.cause(), None);
        assert!(!control.tree_is_cancelled());
        assert!(success.seal());
        assert!(root_control.success_is_sealed());

        assert_eq!(
            root_control.request_cancel(RunCancellationCause::Failure(
                "failure after durable success".to_string()
            )),
            RunCancelOutcome::Rejected
        );
        assert_eq!(child_control.cause(), None);
        assert!(!control.tree_is_cancelled());

        control
            .complete_execution(root_execution, AgentStatus::Completed(None), None)
            .expect("complete root");
        control
            .complete_execution(child_execution, AgentStatus::Completed(None), None)
            .expect("complete child");
    }

    #[test]
    fn root_success_reservation_failure_resolution_closes_tree_before_return() {
        let root_control = RunControl::new();
        let (control, _root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        let success = root_control.begin_success_commit().expect("success commit");
        let failure = RunCancellationCause::Failure(
            "success terminal commit lost durable authority".to_string(),
        );

        assert!(success.abandon_with_cancellation(failure.clone()));

        assert_eq!(root_control.cause(), Some(failure.clone()));
        assert_eq!(child_control.cause(), Some(failure));
        assert!(control.tree_is_cancelled());
        assert!(child_control.begin_tool_effect_admission().is_none());
    }

    #[test]
    fn internal_success_abandonment_preserves_a_pending_stop_as_first_cause() {
        let root_control = RunControl::new();
        let (control, _root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        let success = root_control.begin_success_commit().expect("success commit");

        assert_eq!(
            root_control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
            RunCancelOutcome::Deferred(crate::runtime::RunCancelDeferral {
                primary: crate::runtime::RunReservationKind::SuccessCommit,
                secondary: None,
            })
        );
        assert_eq!(root_control.cause(), None);
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );

        assert!(
            success.abandon_with_cancellation(RunCancellationCause::Failure(
                "internal success commit failure".to_string(),
            ))
        );

        assert_eq!(
            root_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        assert!(control.tree_is_cancelled());
    }

    #[test]
    fn authoritative_success_resolution_uses_exact_durable_cause() {
        let root_control = RunControl::new();
        let (control, _root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        let success = root_control.begin_success_commit().expect("success commit");
        let durable_failure =
            RunCancellationCause::Failure("exact durable failure owner".to_string());

        assert_eq!(
            root_control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
            RunCancelOutcome::Deferred(crate::runtime::RunCancelDeferral {
                primary: crate::runtime::RunReservationKind::SuccessCommit,
                secondary: None,
            })
        );
        assert!(success.resolve_authoritative_cancellation(durable_failure.clone()));

        assert_eq!(root_control.cause(), Some(durable_failure));
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        assert!(control.tree_is_cancelled());
    }

    #[derive(Clone, Copy, Debug)]
    enum DetachedSuccessResolutionCase {
        Authoritative,
        Abandon,
    }

    #[test]
    fn detached_success_resolution_keeps_applied_outcome_and_tree_fanout() {
        for resolution_case in [
            DetachedSuccessResolutionCase::Authoritative,
            DetachedSuccessResolutionCase::Abandon,
        ] {
            let root_control = RunControl::new();
            let (control, root_execution) =
                AgentControl::with_root_control(SessionId::new(), 1, root_control.clone())
                    .expect("agent tree");
            let success = root_control.begin_success_commit().expect("success commit");
            drop(root_execution);
            let cause = RunCancellationCause::Interruption(TurnInterruptionCause::UserStop);

            let resolved = match resolution_case {
                DetachedSuccessResolutionCase::Authoritative => {
                    success.resolve_authoritative_cancellation(cause.clone())
                }
                DetachedSuccessResolutionCase::Abandon => {
                    success.abandon_with_cancellation(cause.clone())
                }
            };

            assert!(resolved, "resolution={resolution_case:?}");
            assert_eq!(root_control.cause(), Some(cause));
            assert!(control.tree_is_cancelled());
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum SuccessCommitReleaseCase {
        Explicit,
        Drop,
    }

    #[test]
    fn releasing_or_dropping_deferred_root_failure_closes_tree_before_return() {
        for release_case in [
            SuccessCommitReleaseCase::Explicit,
            SuccessCommitReleaseCase::Drop,
        ] {
            let root_control = RunControl::new();
            let (control, _root_execution) =
                AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                    .expect("agent tree");
            let (_, child_execution) = control
                .register_child(&AgentPath::root(), "child", SessionId::new(), None)
                .expect("child");
            let child_control = child_execution.run_control();
            let success = root_control.begin_success_commit().expect("success commit");
            let failure =
                RunCancellationCause::Failure(format!("heartbeat failed before {release_case:?}"));

            assert_eq!(
                root_control.request_cancel(failure.clone()),
                RunCancelOutcome::Deferred(crate::runtime::RunCancelDeferral {
                    primary: crate::runtime::RunReservationKind::SuccessCommit,
                    secondary: None,
                })
            );
            assert_eq!(root_control.cause(), None);
            assert_eq!(child_control.cause(), None);
            assert!(!control.tree_is_cancelled());

            match release_case {
                SuccessCommitReleaseCase::Explicit => success.release(),
                SuccessCommitReleaseCase::Drop => drop(success),
            }

            assert_eq!(root_control.cause(), Some(failure.clone()));
            assert_eq!(child_control.cause(), Some(failure));
            assert!(control.tree_is_cancelled());
            assert!(child_control.begin_tool_effect_admission().is_none());
        }
    }

    #[test]
    fn stale_root_terminal_router_is_replaced_when_control_joins_a_new_tree() {
        let root_control = RunControl::new();
        let (first_tree, first_root) =
            AgentControl::with_root_control(SessionId::new(), 1, root_control.clone())
                .expect("first tree");
        drop(first_root);
        drop(first_tree);

        let (second_tree, second_root) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("second tree");
        let (_, child) = second_tree
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("second-tree child");
        let child_control = child.run_control();
        let failure = RunCancellationCause::Failure("second tree failed".to_string());

        assert!(root_control.fail("second tree failed"));
        assert_eq!(root_control.cause(), Some(failure.clone()));
        assert_eq!(child_control.cause(), Some(failure));
        assert!(second_tree.tree_is_cancelled());

        second_tree
            .complete_execution(
                second_root,
                AgentStatus::Errored("root failed".to_string()),
                None,
            )
            .expect("complete root");
        second_tree
            .complete_execution(child, AgentStatus::Errored("tree failed".to_string()), None)
            .expect("complete child");
    }

    #[test]
    fn stale_root_owner_cannot_route_late_failure_into_the_current_turn() {
        let stale_root_control = RunControl::new();
        let (control, stale_root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, stale_root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        assert!(stale_root_control.seal_success());
        drop(stale_root_execution);

        let current_root_control = RunControl::new();
        let _current_root_execution = control
            .try_acquire_execution_with_control(&AgentPath::root(), current_root_control.clone())
            .expect("current root turn");

        assert!(!stale_root_control.fail("late failure from stale root owner"));
        assert!(stale_root_control.success_is_sealed());
        assert_eq!(current_root_control.cause(), None);
        assert_eq!(child_control.cause(), None);
        assert!(!control.tree_is_cancelled());
        child_control
            .begin_tool_effect_admission()
            .expect("current sibling admission remains open")
            .admit()
            .expect("current sibling effect may start");
    }

    #[test]
    fn child_failure_remains_local_and_does_not_close_sibling_effect_admission() {
        let root_control = RunControl::new();
        let (control, root) =
            AgentControl::with_root_control(SessionId::new(), 3, root_control.clone())
                .expect("agent tree");
        let (_, failed_child) = control
            .register_child(&AgentPath::root(), "failed_child", SessionId::new(), None)
            .expect("failed child");
        let (_, sibling) = control
            .register_child(&AgentPath::root(), "sibling", SessionId::new(), None)
            .expect("sibling");
        let failed_child_control = failed_child.run_control();
        let sibling_control = sibling.run_control();
        let failure = RunCancellationCause::Failure("child-only failure".to_string());

        assert!(failed_child_control.fail("child-only failure"));
        assert_eq!(failed_child_control.cause(), Some(failure));
        assert_eq!(root_control.cause(), None);
        assert_eq!(sibling_control.cause(), None);
        assert!(!control.tree_is_cancelled());
        sibling_control
            .begin_tool_effect_admission()
            .expect("sibling admission remains open")
            .admit()
            .expect("sibling effect may start");

        control
            .complete_execution(root, AgentStatus::Completed(None), None)
            .expect("complete root");
        control
            .complete_execution(
                failed_child,
                AgentStatus::Errored("child failed".to_string()),
                None,
            )
            .expect("complete failed child");
        control
            .complete_execution(sibling, AgentStatus::Completed(None), None)
            .expect("complete sibling");
    }

    #[test]
    fn one_live_root_control_cannot_be_attached_to_two_agent_trees() {
        let root_control = RunControl::new();
        let (first_tree, first_root) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("first tree");
        let (_, first_child) = first_tree
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("first-tree child");
        let first_child_control = first_child.run_control();

        let second = AgentControl::with_root_control(SessionId::new(), 1, root_control.clone());
        assert!(matches!(
            second,
            Err(AgentControlError::RunControlOwnedByDifferentTree)
        ));

        let failure = RunCancellationCause::Failure("first tree still owns failure".to_string());
        assert!(root_control.fail("first tree still owns failure"));
        assert_eq!(first_child_control.cause(), Some(failure));
        assert!(first_tree.tree_is_cancelled());
        first_tree
            .complete_execution(
                first_root,
                AgentStatus::Errored("root failed".to_string()),
                None,
            )
            .expect("complete root");
        first_tree
            .complete_execution(
                first_child,
                AgentStatus::Errored("tree failed".to_string()),
                None,
            )
            .expect("complete child");
    }

    #[test]
    fn same_tree_can_reacquire_its_root_control_with_the_existing_router() {
        let root_control = RunControl::new();
        let (control, root) =
            AgentControl::with_root_control(SessionId::new(), 1, root_control.clone())
                .expect("agent tree");
        drop(root);

        let reacquired = control
            .try_acquire_execution_with_control(&AgentPath::root(), root_control)
            .expect("same-tree root continuation");
        drop(reacquired);
    }

    #[test]
    fn a_different_root_terminal_cause_blocks_every_competing_tree_producer() {
        let cases = [
            (
                RunCancellationCause::Failure("first failure".to_string()),
                TreeTerminalProducer::UserStop,
            ),
            (
                RunCancellationCause::Interruption(TurnInterruptionCause::UserStop),
                TreeTerminalProducer::ApprovalAbort,
            ),
            (
                RunCancellationCause::Superseded,
                TreeTerminalProducer::Failure,
            ),
            (
                RunCancellationCause::Failure("first failure".to_string()),
                TreeTerminalProducer::Superseded,
            ),
        ];
        for (existing_cause, producer) in cases {
            let root_control = RunControl::new();
            assert!(root_control.cancel(existing_cause.clone()));
            let (control, _root_execution) =
                AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                    .expect("agent tree");
            let (_, child_execution) = control
                .register_child(&AgentPath::root(), "child", SessionId::new(), None)
                .expect("child");

            assert!(!apply_tree_terminal_producer(&control, producer));
            assert_eq!(root_control.cause(), Some(existing_cause));
            assert_eq!(child_execution.run_control().cause(), None);
            assert!(!control.tree_is_cancelled());
        }
    }

    #[test]
    fn explicit_stop_uses_the_tree_owner_as_soon_as_root_success_is_durable() {
        let root_control = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                .expect("agent tree");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_control = child_execution.run_control();
        assert!(root_control.seal_success());

        assert!(control.interrupt_tree(TurnInterruptionCause::UserStop));
        assert_eq!(root_control.cause(), None);
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        control
            .complete_execution(root_execution, AgentStatus::Completed(None), None)
            .expect("complete root");
    }

    #[test]
    fn restored_status_without_a_worker_does_not_claim_live_tree_ownership() {
        for stale_status in [AgentStatus::PendingInit, AgentStatus::Running] {
            let root_control = RunControl::new();
            let (control, root_execution) =
                AgentControl::with_root_control(SessionId::new(), 2, root_control.clone())
                    .expect("agent tree");
            let restored = control
                .restore_inactive_child(
                    &AgentPath::root(),
                    "restored",
                    SessionId::new(),
                    stale_status.clone(),
                    None,
                )
                .expect("restore inactive child");
            assert!(!restored.is_active);
            assert!(root_control.seal_success());

            assert!(!control.interrupt_tree(TurnInterruptionCause::UserStop));
            assert!(!control.tree_is_cancelled());
            assert_eq!(
                control.status(&restored.path).expect("restored status"),
                stale_status
            );

            control
                .complete_execution(root_execution, AgentStatus::Completed(None), None)
                .expect("complete root");
        }
    }

    #[test]
    fn agent_paths_are_canonical_and_resolve_relative_or_absolute_references() {
        let worker = AgentPath::root().join("worker_1").expect("worker path");
        assert_eq!(worker.as_str(), "/root/worker_1");
        assert_eq!(worker.name(), "worker_1");
        assert_eq!(worker.parent(), Some(AgentPath::root()));
        assert_eq!(
            worker.resolve("reviewer").expect("relative path"),
            AgentPath::try_from("/root/worker_1/reviewer").expect("canonical path")
        );
        assert_eq!(
            worker.resolve("/root/other").expect("absolute path"),
            AgentPath::try_from("/root/other").expect("canonical path")
        );

        assert!(AgentPath::root().join("BadName").is_err());
        assert!(AgentPath::root().join("two/parts").is_err());
        assert!(AgentPath::try_from("/other/worker").is_err());
        assert!(AgentPath::try_from("/root/worker/").is_err());
        assert!(AgentPath::root().resolve("../sibling").is_err());
    }

    #[test]
    fn root_and_children_share_a_bounded_raii_execution_pool() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (_, first_execution) = control
            .register_child(&root, "first", SessionId::new(), None)
            .expect("first child");

        let second_result = control.register_child(&root, "second", SessionId::new(), None);
        assert!(matches!(
            second_result,
            Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: 2
            })
        ));
        assert_eq!(control.snapshot().expect("snapshot").active_agent_count, 2);

        drop(first_execution);
        let (_, second_execution) = control
            .register_child(&root, "second", SessionId::new(), None)
            .expect("second child after release");
        assert_eq!(control.snapshot().expect("snapshot").active_agent_count, 2);

        drop(root_execution);
        assert_eq!(control.snapshot().expect("snapshot").active_agent_count, 1);
        drop(second_execution);
        assert_eq!(control.snapshot().expect("snapshot").active_agent_count, 0);
    }

    #[test]
    fn snapshots_derive_tree_links_spawn_order_status_and_activity() {
        let root_session_id = SessionId::new();
        let (control, _root_execution) = AgentControl::new(root_session_id, 4).expect("agent tree");
        let root = AgentPath::root();
        let first_session_id = SessionId::new();
        let (first, _first_execution) = control
            .register_child(
                &root,
                "research",
                first_session_id,
                Some("Inspect runtime".to_string()),
            )
            .expect("research child");
        let (second, _second_execution) = control
            .register_child(&root, "review", SessionId::new(), None)
            .expect("review child");
        let (nested, _nested_execution) = control
            .register_child(&first.path, "tests", SessionId::new(), None)
            .expect("nested child");

        control
            .set_status(
                &first.path,
                AgentStatus::Completed(Some("runtime inspected".to_string())),
            )
            .expect("status");
        control
            .set_activity(&first.path, Some("Reported findings".to_string()))
            .expect("activity");

        let snapshot = control.snapshot().expect("tree snapshot");
        assert_eq!(
            snapshot
                .agents
                .iter()
                .map(|agent| agent.spawn_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            snapshot.agents[0].children,
            vec![first.path.clone(), second.path]
        );
        assert_eq!(snapshot.agents[1].children, vec![nested.path]);
        assert_eq!(
            snapshot.agents[1].last_activity.as_deref(),
            Some("Reported findings")
        );
        assert_eq!(
            control
                .path_for_session(root_session_id)
                .expect("root path"),
            Some(root)
        );
        assert_eq!(
            control
                .path_for_session(first_session_id)
                .expect("child path"),
            Some(first.path)
        );
    }

    #[tokio::test]
    async fn mailbox_preserves_fifo_order_and_notifies_by_generation() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, _child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let mut activity = control.subscribe_mailbox(&root).expect("subscription");

        assert_eq!(
            control
                .enqueue_mail(AgentMailboxMessage::new(
                    child.path.clone(),
                    root.clone(),
                    "one",
                    false,
                ))
                .expect("first mail"),
            1
        );
        assert_eq!(
            control
                .enqueue_mail(AgentMailboxMessage::new(
                    child.path,
                    root.clone(),
                    "two",
                    true,
                ))
                .expect("second mail"),
            2
        );

        activity.changed().await.expect("mailbox activity");
        assert_eq!(*activity.borrow_and_update(), 2);
        assert_eq!(
            control
                .wait_for_mailbox_activity(&root, 0)
                .await
                .expect("observed generation"),
            2
        );
        let drained = control.drain_mailbox(&root).expect("drain mailbox");
        assert_eq!(
            drained
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        let root_snapshot = control
            .list_agents(Some(&root))
            .expect("root subtree")
            .into_iter()
            .next()
            .expect("root snapshot");
        assert_eq!(root_snapshot.mailbox_generation, 2);
        assert_eq!(root_snapshot.pending_mail_count, 0);
    }

    #[test]
    fn durable_mailbox_commit_is_validated_and_enqueued_as_one_control_operation() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, _child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");

        let error = match control.enqueue_mail_after_durable_commit(
            AgentMailboxMessage::new(child.path.clone(), root.clone(), "not durable", false),
            false,
            || Err("injected sqlite failure".to_string()),
        ) {
            Err(error) => error,
            Ok(_) => panic!("failed durable commit must reject the mailbox write"),
        };
        assert!(matches!(
            error,
            AgentControlError::DurableMailboxCommit(message)
                if message == "injected sqlite failure"
        ));
        let unchanged = control
            .list_agents(Some(&root))
            .expect("root snapshot")
            .into_iter()
            .next()
            .expect("root");
        assert_eq!(unchanged.mailbox_generation, 0);
        assert_eq!(unchanged.pending_mail_count, 0);

        let outcome = control
            .enqueue_mail_after_durable_commit(
                AgentMailboxMessage::new(child.path, root.clone(), "durable", false),
                false,
                || Ok(()),
            )
            .expect("durable mail");
        let AgentMailDeliveryOutcome::Enqueued {
            generation,
            scheduled,
        } = outcome
        else {
            panic!("ordinary durable mail must be enqueued");
        };
        assert_eq!(generation, 1);
        assert!(scheduled.is_empty());
        assert_eq!(
            control
                .drain_mailbox(&root)
                .expect("mailbox")
                .into_iter()
                .map(|message| message.content)
                .collect::<Vec<_>>(),
            vec!["durable"]
        );
    }

    #[test]
    fn blocked_durable_commit_does_not_block_tree_list_or_cancel() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, _child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let child_path = child.path.clone();
        let (commit_entered_tx, commit_entered_rx) = std::sync::mpsc::channel();
        let (release_commit_tx, release_commit_rx) = std::sync::mpsc::channel();
        let sender_control = control.clone();
        let sender_root = root.clone();
        let sender = std::thread::spawn(move || {
            sender_control.enqueue_mail_after_durable_commit(
                AgentMailboxMessage::new(child_path, sender_root, "durable", false),
                false,
                || {
                    commit_entered_tx.send(()).expect("commit entered signal");
                    release_commit_rx.recv().expect("release durable commit");
                    Ok(())
                },
            )
        });
        commit_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("durable commit entered");

        let observer_control = control.clone();
        let observer_root = root.clone();
        let observer_child = child.path.clone();
        let (observer_tx, observer_rx) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            let result = observer_control
                .list_agents(Some(&observer_root))
                .and_then(|agents| {
                    if agents.len() != 2 {
                        return Err(AgentControlError::AgentNotFound(observer_child.clone()));
                    }
                    observer_control.cancel_agent(&observer_child)
                });
            observer_tx.send(result).expect("observer result");
        });
        let observed = observer_rx.recv_timeout(std::time::Duration::from_secs(1));
        release_commit_tx.send(()).expect("release commit");
        let sender_result = sender.join().expect("sender thread");
        observer.join().expect("observer thread");

        observed
            .expect("list/cancel must remain responsive while durable commit is blocked")
            .expect("list/cancel result");
        let _ = sender_result.expect("durable mail delivery");
    }

    #[test]
    fn tree_stop_during_durable_commit_suppresses_the_committed_trigger() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        drop(child_execution);
        let (commit_entered_tx, commit_entered_rx) = std::sync::mpsc::channel();
        let (release_commit_tx, release_commit_rx) = std::sync::mpsc::channel();
        let committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sender_committed = Arc::clone(&committed);
        let sender_control = control.clone();
        let sender_root = root.clone();
        let child_path = child.path.clone();
        let sender = std::thread::spawn(move || {
            sender_control.enqueue_mail_after_durable_commit(
                AgentMailboxMessage::new(sender_root, child_path, "follow-up", true),
                true,
                || {
                    commit_entered_tx.send(()).expect("commit entered signal");
                    release_commit_rx.recv().expect("release durable commit");
                    sender_committed.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
            )
        });
        commit_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("durable commit entered");

        control.interrupt_tree(TurnInterruptionCause::UserStop);
        release_commit_tx.send(()).expect("release commit");
        let outcome = sender
            .join()
            .expect("sender thread")
            .expect("durable evidence remains committed");

        assert!(committed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(matches!(outcome, AgentMailDeliveryOutcome::Suppressed));
        assert!(
            !control
                .mailbox_has_trigger_turn(&child.path)
                .expect("trigger state")
        );
        let child = control
            .list_agents(Some(&child.path))
            .expect("child snapshot")
            .into_iter()
            .next()
            .expect("child");
        assert_eq!(child.pending_mail_count, 0);
        assert!(!child.is_active);
    }

    #[test]
    fn durable_child_terminal_during_commit_suppresses_restart_and_trigger() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let child_cancel = child_execution.cancel_token();
        drop(child_execution);
        let (commit_entered_tx, commit_entered_rx) = std::sync::mpsc::channel();
        let (release_commit_tx, release_commit_rx) = std::sync::mpsc::channel();
        let sender_control = control.clone();
        let sender_root = root.clone();
        let sender_child = child.path.clone();
        let sender = std::thread::spawn(move || {
            sender_control.enqueue_mail_after_durable_commit(
                AgentMailboxMessage::new(sender_root, sender_child, "follow-up", true),
                true,
                || {
                    commit_entered_tx.send(()).expect("commit entered signal");
                    release_commit_rx.recv().expect("release durable commit");
                    Ok(())
                },
            )
        });
        commit_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("durable commit entered");
        let terminal_control = control.clone();
        let terminal_child = child.path.clone();
        let terminal = std::thread::spawn(move || {
            terminal_control.cancel_for_durable_terminal(&terminal_child)
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !child_cancel.is_cancelled() {
            assert!(
                std::time::Instant::now() < deadline,
                "durable terminal cancellation must precede the mailbox purge reservation"
            );
            std::thread::yield_now();
        }

        release_commit_tx.send(()).expect("release commit");
        let outcome = sender
            .join()
            .expect("sender thread")
            .expect("durable evidence remains committed");
        terminal
            .join()
            .expect("terminal thread")
            .expect("durable terminal purge");

        assert!(matches!(outcome, AgentMailDeliveryOutcome::Suppressed));
        assert!(
            !control
                .mailbox_has_trigger_turn(&child.path)
                .expect("trigger state")
        );
        let child = control
            .list_agents(Some(&child.path))
            .expect("child snapshot")
            .into_iter()
            .next()
            .expect("child");
        assert_eq!(child.pending_mail_count, 0);
        assert!(!child.is_active);
    }

    #[test]
    fn node_tokens_can_be_refreshed_and_tree_cancellation_cascades() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let first_child_cancel = child_execution.cancel_token();

        control.cancel_agent(&child.path).expect("cancel child");
        assert!(first_child_cancel.is_cancelled());
        drop(child_execution);

        let restarted = control
            .try_acquire_execution(&child.path)
            .expect("restart child");
        assert!(!restarted.cancel_token().is_cancelled());
        control.interrupt_tree(TurnInterruptionCause::UserStop);
        assert!(root_execution.cancel_token().is_cancelled());
        assert!(restarted.cancel_token().is_cancelled());
        drop(restarted);
        assert!(matches!(
            control.try_acquire_execution(&child.path),
            Err(AgentControlError::TreeCancelled)
        ));
    }

    #[test]
    fn durable_terminal_cancel_purges_child_triggers_and_promotes_root_to_tree_cancel() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        control
            .enqueue_mail(AgentMailboxMessage::new(
                root.clone(),
                child.path.clone(),
                "informational",
                false,
            ))
            .expect("informational mail");
        control
            .enqueue_mail(AgentMailboxMessage::new(
                root.clone(),
                child.path.clone(),
                "follow-up",
                true,
            ))
            .expect("trigger mail");

        control
            .cancel_for_durable_terminal(&child.path)
            .expect("durable child terminal");
        assert!(child_execution.cancel_token().is_cancelled());
        assert!(!root_execution.cancel_token().is_cancelled());
        assert!(!control.tree_is_cancelled());
        let restored = control
            .list_agents(Some(&child.path))
            .expect("child snapshot")
            .into_iter()
            .next()
            .expect("child");
        assert_eq!(restored.pending_mail_count, 1);
        assert!(
            !control
                .mailbox_has_trigger_turn(&child.path)
                .expect("trigger state")
        );

        control
            .cancel_for_durable_terminal(&root)
            .expect("durable root terminal");
        assert!(root_execution.cancel_token().is_cancelled());
        assert!(control.tree_is_cancelled());
    }

    #[test]
    fn concurrent_durable_terminal_purges_converge_and_allow_later_followup() {
        for terminal_status in [AgentStatus::Completed(None), AgentStatus::Interrupted] {
            let (control, _root_execution) =
                AgentControl::new(SessionId::new(), 2).expect("agent tree");
            let root = AgentPath::root();
            let (child, child_execution) = control
                .register_child(&root, "worker", SessionId::new(), None)
                .expect("worker");
            control
                .enqueue_mail(AgentMailboxMessage::new(
                    root.clone(),
                    child.path.clone(),
                    "stale follow-up",
                    true,
                ))
                .expect("stale trigger");

            let delivery = control
                .lock_mail_delivery()
                .expect("hold delivery reservation");
            let terminals = (0..2)
                .map(|_| {
                    let terminal_control = control.clone();
                    let terminal_path = child.path.clone();
                    std::thread::spawn(move || {
                        terminal_control.cancel_for_durable_terminal(&terminal_path)
                    })
                })
                .collect::<Vec<_>>();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                let pending = control
                    .lock()
                    .expect("agent registry")
                    .agents
                    .get(&child.path)
                    .expect("child entry")
                    .trigger_purge_pending;
                if pending == 2 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "both terminal requests must enter the shared purge epoch"
                );
                std::thread::yield_now();
            }

            drop(delivery);
            for terminal in terminals {
                terminal
                    .join()
                    .expect("terminal thread")
                    .expect("durable terminal purge");
            }
            {
                let state = control.lock().expect("converged agent registry");
                let agent = state.agents.get(&child.path).expect("child entry");
                assert_eq!(agent.trigger_purge_pending, 0);
                assert!(!agent.mailbox.iter().any(|message| message.trigger_turn));
            }

            let scheduled_after_terminal = control
                .complete_execution(child_execution, terminal_status.clone(), None)
                .expect("complete terminal child execution");
            assert!(scheduled_after_terminal.is_empty());
            assert_eq!(
                control.status(&child.path).expect("terminal child status"),
                terminal_status
            );

            let outcome = control
                .enqueue_mail_after_durable_commit(
                    AgentMailboxMessage::new(root, child.path.clone(), "new follow-up", true),
                    true,
                    || Ok(()),
                )
                .expect("follow-up after converged purges");
            let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = outcome else {
                panic!("a later follow-up must be enqueued after all purges complete");
            };
            assert_eq!(scheduled.len(), 1);
            assert!(
                control
                    .mailbox_has_trigger_turn(&child.path)
                    .expect("new trigger")
            );
            drop(scheduled);
        }
    }

    #[test]
    fn durable_terminal_wait_does_not_purge_a_replacement_at_the_same_path() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let old_session_id = SessionId::new();
        let (child, child_execution) = control
            .register_child(&root, "worker", old_session_id, None)
            .expect("original worker");
        let old_cancel = child_execution.cancel_token();
        let delivery = control
            .lock_mail_delivery()
            .expect("hold delivery reservation");
        let terminal_control = control.clone();
        let terminal_path = child.path.clone();
        let terminal = std::thread::spawn(move || {
            terminal_control.cancel_for_durable_terminal(&terminal_path)
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !old_cancel.is_cancelled() {
            assert!(
                std::time::Instant::now() < deadline,
                "terminal cancellation must enter its first phase"
            );
            std::thread::yield_now();
        }

        {
            let mut state = control.lock().expect("agent registry");
            let removed = state
                .agents
                .remove(&child.path)
                .expect("remove original worker during delivery wait");
            assert_eq!(removed.session_id, old_session_id);
        }
        let replacement_session_id = SessionId::new();
        let (replacement, replacement_execution) = control
            .register_child(&root, "worker", replacement_session_id, None)
            .expect("replacement worker");
        {
            let mut state = control.lock().expect("replacement registry");
            let replacement_entry = state
                .agents
                .get_mut(&replacement.path)
                .expect("replacement entry");
            replacement_entry
                .mailbox
                .push_back(AgentMailboxMessage::new(
                    root,
                    replacement.path.clone(),
                    "replacement trigger",
                    true,
                ));
            replacement_entry.mailbox_generation =
                replacement_entry.mailbox_generation.wrapping_add(1);
            replacement_entry
                .mailbox_activity_tx
                .send_replace(replacement_entry.mailbox_generation);
        }

        drop(delivery);
        terminal
            .join()
            .expect("terminal thread")
            .expect("old terminal cancellation");

        let retained = control
            .list_agents(Some(&replacement.path))
            .expect("replacement snapshot")
            .into_iter()
            .next()
            .expect("replacement worker");
        assert_eq!(retained.session_id, replacement_session_id);
        assert_eq!(retained.pending_mail_count, 1);
        assert!(
            control
                .mailbox_has_trigger_turn(&replacement.path)
                .expect("replacement trigger")
        );
        assert!(!replacement_execution.cancel_token().is_cancelled());
        drop(child_execution);
        drop(replacement_execution);
    }

    #[test]
    fn ordinary_interrupt_keeps_trigger_mail_for_a_later_followup_turn() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        control
            .enqueue_mail(AgentMailboxMessage::new(
                root,
                child.path.clone(),
                "follow-up",
                true,
            ))
            .expect("trigger mail");

        control
            .cancel_agent(&child.path)
            .expect("ordinary interrupt");
        assert!(child_execution.cancel_token().is_cancelled());
        assert!(
            control
                .mailbox_has_trigger_turn(&child.path)
                .expect("trigger state")
        );
    }
}
