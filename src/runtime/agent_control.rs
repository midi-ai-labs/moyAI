use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::session::SessionId;

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
}

#[derive(Clone)]
pub struct AgentControl {
    inner: Arc<AgentControlInner>,
}

struct AgentControlInner {
    tree_cancel: CancellationToken,
    state: Mutex<AgentTreeState>,
    mail_delivery: Mutex<()>,
    activity_tx: watch::Sender<u64>,
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
    node_cancel: CancellationToken,
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
    cancel: CancellationToken,
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
        if max_concurrent_agents == 0 {
            return Err(AgentControlError::InvalidCapacity);
        }

        let tree_cancel = CancellationToken::new();
        let (activity_tx, _) = watch::channel(0);
        let root_cancel = tree_cancel.child_token();
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
                node_cancel: root_cancel,
                mailbox: VecDeque::new(),
                mailbox_generation: 0,
                trigger_admission_epoch: 0,
                trigger_purge_pending: 0,
                mailbox_activity_tx,
            },
        );
        let control = Self {
            inner: Arc::new(AgentControlInner {
                tree_cancel,
                state: Mutex::new(AgentTreeState {
                    max_concurrent_agents,
                    agents,
                }),
                mail_delivery: Mutex::new(()),
                activity_tx,
            }),
        };
        let root_execution = control.try_acquire_execution(&root)?;
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
        if self.inner.tree_cancel.is_cancelled() {
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
        let node_cancel = self.inner.tree_cancel.child_token();
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
                node_cancel: node_cancel.clone(),
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
                cancel: node_cancel,
            },
        ))
    }

    pub fn try_acquire_execution(
        &self,
        path: &AgentPath,
    ) -> Result<AgentExecutionLease, AgentControlError> {
        let mut state = self.lock()?;
        if self.inner.tree_cancel.is_cancelled() {
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

        let marker = Arc::new(());
        let node_cancel = self.inner.tree_cancel.child_token();
        let agent = state
            .agents
            .get_mut(path)
            .expect("agent existence was checked while holding the same registry lock");
        agent.execution_marker = Some(Arc::clone(&marker));
        agent.node_cancel = node_cancel.clone();
        drop(state);
        self.notify_activity();

        Ok(AgentExecutionLease {
            control: self.clone(),
            path: path.clone(),
            marker,
            cancel: node_cancel,
        })
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
        let node_cancel = self.inner.tree_cancel.child_token();
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
                node_cancel,
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
            if schedule_triggered && self.inner.tree_cancel.is_cancelled() {
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
            && (self.inner.tree_cancel.is_cancelled()
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
        let scheduled = if self.inner.tree_cancel.is_cancelled() {
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
        agent.node_cancel.cancel();
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
            && (self.inner.tree_cancel.is_cancelled()
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
        let cancel = {
            let state = self.lock()?;
            let agent = state
                .agents
                .get(path)
                .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
            if agent.execution_marker.is_none() {
                return Err(AgentControlError::AgentNotActive(path.clone()));
            }
            agent.node_cancel.clone()
        };
        cancel.cancel();
        self.notify_activity();
        Ok(())
    }

    /// Stops work whose durable session was terminalized outside the current worker.
    /// Unlike `cancel_agent`, a child cannot restart from an already queued trigger turn.
    pub fn cancel_for_durable_terminal(&self, path: &AgentPath) -> Result<(), AgentControlError> {
        if path.is_root() {
            self.cancel_tree();
            return Ok(());
        }

        let (terminal_session_id, terminal_epoch) = {
            let mut state = self.lock()?;
            let agent = state
                .agents
                .get_mut(path)
                .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
            agent.node_cancel.cancel();
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

    pub fn cancel_tree(&self) {
        self.inner.tree_cancel.cancel();
        self.notify_activity();
    }

    pub fn tree_cancel_token(&self) -> CancellationToken {
        self.inner.tree_cancel.clone()
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
            let cancel = self.inner.tree_cancel.child_token();
            let agent = state
                .agents
                .get_mut(&path)
                .expect("scheduled agent was selected from this registry");
            agent.execution_marker = Some(marker.clone());
            agent.node_cancel = cancel.clone();
            agent.status = AgentStatus::PendingInit;
            leases.push(AgentExecutionLease {
                control: self.clone(),
                path,
                marker,
                cancel,
            });
        }
        leases
    }

    fn notify_activity(&self) {
        self.inner
            .activity_tx
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

impl AgentExecutionLease {
    pub fn path(&self) -> &AgentPath {
        &self.path
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
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

        control.cancel_tree();
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
        control.cancel_tree();
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
        assert!(!control.tree_cancel_token().is_cancelled());
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
        assert!(control.tree_cancel_token().is_cancelled());
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
