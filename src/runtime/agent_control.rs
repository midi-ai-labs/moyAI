use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::protocol::{HistoryItemId, TurnInterruptionCause};
use crate::runtime::cancel::{RunTerminalRoute, RunTerminalRouteKind};
use crate::runtime::{RunCancelOutcome, RunControl};
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
        Some(Self::root())
    }

    pub fn join(&self, task_name: &str) -> Result<Self, String> {
        if !self.is_root() {
            return Err("only the root agent can own a direct child".to_string());
        }
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
        Self::root().join(reference)
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
    if let Some(segment) = segments.next() {
        validate_task_name(segment)?;
    }
    if segments.next().is_some() {
        return Err("agent paths are limited to `/root` and one direct child".to_string());
    }
    Ok(())
}

fn validate_relative_reference(reference: &str) -> Result<(), String> {
    if reference.contains('/') {
        return Err("relative agent references must be one direct-child name".to_string());
    }
    validate_task_name(reference)
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
}

/// Lifecycle states that can be published while an exact execution lease is active.
///
/// Keeping this separate from [`InactiveAgentStatus`] prevents one owner from projecting a
/// terminal status while still retaining an active execution marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveAgentStatus {
    PendingInit,
    Running,
}

impl From<ActiveAgentStatus> for AgentStatus {
    fn from(status: ActiveAgentStatus) -> Self {
        match status {
            ActiveAgentStatus::PendingInit => Self::PendingInit,
            ActiveAgentStatus::Running => Self::Running,
        }
    }
}

/// Lifecycle states that can be retained only after an exact execution lease is released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InactiveAgentStatus {
    Interrupted,
    Completed(Option<String>),
    Errored(String),
    Shutdown,
}

impl From<InactiveAgentStatus> for AgentStatus {
    fn from(status: InactiveAgentStatus) -> Self {
        match status {
            InactiveAgentStatus::Interrupted => Self::Interrupted,
            InactiveAgentStatus::Completed(result) => Self::Completed(result),
            InactiveAgentStatus::Errored(message) => Self::Errored(message),
            InactiveAgentStatus::Shutdown => Self::Shutdown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMailboxNotice {
    pub history_item_id: HistoryItemId,
    pub trigger_turn: bool,
    pub generation: u64,
}

const MAX_AGENT_MAILBOX_NOTICES: usize = 128;

/// Root plus direct children retained by one process-local tree.
///
/// Durable child sessions remain owned by storage. Bounding the live registry prevents a long
/// root task from turning snapshots, cancellation fan-out, and mailbox bookkeeping into an
/// unbounded in-memory projection.
pub const MAX_RETAINED_AGENTS: usize = 256;

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
    #[error(
        "max_concurrent_agents {requested} is outside the supported range 1..={max_retained_agents}"
    )]
    InvalidCapacity {
        requested: usize,
        max_retained_agents: usize,
    },
    #[error("invalid agent path: {0}")]
    InvalidPath(String),
    #[error("agent `{0}` was not found")]
    AgentNotFound(AgentPath),
    #[error("agent `{0}` already exists")]
    AgentAlreadyExists(AgentPath),
    #[error("agent `{0}` cannot own a child; every child belongs directly to `/root`")]
    InvalidChildParent(AgentPath),
    #[error("session {0} is already registered in this agent tree")]
    SessionAlreadyRegistered(SessionId),
    #[error("agent `{0}` already has an active turn")]
    AgentAlreadyActive(AgentPath),
    #[error("agent `{0}` was shut down and cannot acquire another turn")]
    AgentShutdown(AgentPath),
    #[error("agent `{0}` has no active turn to cancel")]
    AgentNotActive(AgentPath),
    #[error("agent limit reached (root included; max {max_concurrent_agents})")]
    AgentLimitReached { max_concurrent_agents: usize },
    #[error("agent tree reached its retained-agent capacity of {max_retained_agents}")]
    AgentRegistryFull { max_retained_agents: usize },
    #[error("agent spawn-order sequence is exhausted")]
    SpawnOrderExhausted,
    #[error("the agent tree has been cancelled")]
    TreeCancelled,
    #[error("mailbox for agent `{0}` closed")]
    MailboxClosed(AgentPath),
    #[error("mailbox for agent `{recipient}` reached its capacity of {capacity} durable notices")]
    MailboxFull {
        recipient: AgentPath,
        capacity: usize,
    },
    #[error("durable mailbox commit failed: {0}")]
    DurableMailboxCommit(String),
    #[error("agent control lock was poisoned")]
    LockPoisoned,
    #[error("agent `{0}` execution lease is stale")]
    StaleExecution(AgentPath),
    #[error("the root agent cannot be removed from its tree")]
    RootAgentCannotBeRemoved,
    #[error("agent `{0}` is not an uncommitted child registration")]
    AgentRollbackRejected(AgentPath),
    #[error("the root run control is already owned by a different live agent tree")]
    RunControlOwnedByDifferentTree,
    #[error("root turns must be acquired through a retained root scope")]
    RootTurnRequiresScope,
    #[error("agent concurrency can be reconfigured only while the retained tree is quiescent")]
    TreeNotQuiescent,
}

#[derive(Clone)]
pub struct AgentControl {
    inner: Arc<AgentControlInner>,
}

struct AgentControlInner {
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
    next_spawn_order: u64,
    root_scope_control: RunControl,
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
    mailbox: VecDeque<AgentMailboxNotice>,
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

/// Cloneable, non-owning capability for mutations that belong to one exact execution lease.
///
/// Keeping this scope alive does not keep an execution active. Once its owning lease is completed
/// or dropped, or a later turn replaces it, mutations through the stale scope fail closed.
#[derive(Clone)]
pub struct AgentExecutionScope {
    control: AgentControl,
    path: AgentPath,
    marker: Weak<()>,
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

    /// Creates a retained root task scope and a distinct control for its first turn.
    ///
    /// `root_scope_control` is the surface-owned Stop handle for the whole task. The returned
    /// execution lease always owns a fresh, turn-scoped [`RunControl`].
    pub fn with_root_control(
        root_session_id: SessionId,
        max_concurrent_agents: usize,
        root_scope_control: RunControl,
    ) -> Result<(Self, AgentExecutionLease), AgentControlError> {
        if !(1..=MAX_RETAINED_AGENTS).contains(&max_concurrent_agents) {
            return Err(AgentControlError::InvalidCapacity {
                requested: max_concurrent_agents,
                max_retained_agents: MAX_RETAINED_AGENTS,
            });
        }
        if root_scope_control.is_cancelled() || root_scope_control.success_is_sealed() {
            return Err(AgentControlError::TreeCancelled);
        }

        let root_turn_control = RunControl::new();
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
                run_control: root_turn_control.clone(),
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
                root_terminal_router,
                state: Mutex::new(AgentTreeState {
                    max_concurrent_agents,
                    next_spawn_order: 1,
                    root_scope_control: root_scope_control.clone(),
                    agents,
                }),
                mail_delivery: Mutex::new(()),
                activity_tx,
            }
        });
        let control = Self { inner };
        control.install_root_terminal_router(&root_scope_control)?;
        control.install_root_terminal_router(&root_turn_control)?;
        if root_scope_control.is_cancelled() {
            return Err(AgentControlError::TreeCancelled);
        }
        let marker = Arc::new(());
        control
            .lock()?
            .agents
            .get_mut(&root)
            .expect("a newly created agent tree must retain its root")
            .execution_marker = Some(marker.clone());
        let root_execution = AgentExecutionLease {
            control: control.clone(),
            path: root,
            marker,
            run_control: root_turn_control,
        };
        Ok((control, root_execution))
    }

    pub fn register_child(
        &self,
        parent: &AgentPath,
        task_name: &str,
        session_id: SessionId,
        initial_activity: Option<String>,
    ) -> Result<(AgentSnapshot, AgentExecutionLease), AgentControlError> {
        if !parent.is_root() {
            return Err(AgentControlError::InvalidChildParent(parent.clone()));
        }
        let child_path = parent
            .join(task_name)
            .map_err(AgentControlError::InvalidPath)?;
        let mut state = self.lock()?;
        if state.root_scope_control.is_cancelled() {
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
        if state.agents.len() >= MAX_RETAINED_AGENTS {
            return Err(AgentControlError::AgentRegistryFull {
                max_retained_agents: MAX_RETAINED_AGENTS,
            });
        }
        if active_agent_count(&state) >= state.max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: state.max_concurrent_agents,
            });
        }

        let spawn_order = allocate_spawn_order(&mut state)?;
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
        if path.is_root() {
            return Err(AgentControlError::RootTurnRequiresScope);
        }
        let run_control = RunControl::new();
        let mut state = self.lock()?;
        if state.root_scope_control.is_cancelled() {
            return Err(AgentControlError::TreeCancelled);
        }
        let agent = state
            .agents
            .get(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        if agent.execution_marker.is_some() {
            return Err(AgentControlError::AgentAlreadyActive(path.clone()));
        }
        if matches!(agent.status, AgentStatus::Shutdown) {
            return Err(AgentControlError::AgentShutdown(path.clone()));
        }
        if active_agent_count(&state) >= state.max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: state.max_concurrent_agents,
            });
        }

        let marker = Arc::new(());
        let agent = state
            .agents
            .get_mut(path)
            .expect("agent existence was checked while holding the same registry lock");
        agent.execution_marker = Some(Arc::clone(&marker));
        agent.run_control = run_control.clone();
        agent.status = ActiveAgentStatus::PendingInit.into();
        drop(state);
        self.notify_activity();

        Ok(AgentExecutionLease {
            control: self.clone(),
            path: path.clone(),
            marker,
            run_control,
        })
    }

    /// Starts a new user-requested root task on a retained, quiescent agent tree.
    ///
    /// This replaces the previous task scope and creates a fresh turn owner. Idle goal
    /// continuation must use [`Self::try_acquire_root_continuation`] instead so a stale task scope
    /// cannot claim the next turn.
    pub fn try_acquire_root_execution(
        &self,
        root_scope_control: RunControl,
    ) -> Result<AgentExecutionLease, AgentControlError> {
        let root_path = AgentPath::root();
        let mut state = self.lock()?;
        if state.root_scope_control.is_cancelled() {
            return Err(AgentControlError::TreeCancelled);
        }
        if state.agents.values().any(agent_has_live_work) {
            return Err(AgentControlError::AgentAlreadyActive(root_path));
        }
        if root_scope_control.is_cancelled() || root_scope_control.success_is_sealed() {
            return Err(AgentControlError::TreeCancelled);
        }
        if active_agent_count(&state) >= state.max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: state.max_concurrent_agents,
            });
        }

        let root_turn_control = RunControl::new();
        self.install_root_terminal_router(&root_scope_control)?;
        self.install_root_terminal_router(&root_turn_control)?;
        if root_scope_control.is_cancelled() {
            return Err(AgentControlError::TreeCancelled);
        }
        let marker = Arc::new(());
        state.root_scope_control = root_scope_control;
        let root = state
            .agents
            .get_mut(&root_path)
            .expect("an agent tree must retain its root");
        root.execution_marker = Some(marker.clone());
        root.run_control = root_turn_control.clone();
        root.status = AgentStatus::PendingInit;
        drop(state);
        self.notify_activity();

        Ok(AgentExecutionLease {
            control: self.clone(),
            path: root_path,
            marker,
            run_control: root_turn_control,
        })
    }

    pub fn try_acquire_root_continuation(
        &self,
        root_scope_control: RunControl,
    ) -> Result<AgentRootContinuationOutcome, AgentControlError> {
        let root_path = AgentPath::root();
        let mut state = self.lock()?;
        if !state.root_scope_control.same_owner(&root_scope_control) {
            return Ok(AgentRootContinuationOutcome::Invalid);
        }
        if state.root_scope_control.is_cancelled() {
            return Ok(AgentRootContinuationOutcome::Blocked);
        }
        let root = state
            .agents
            .get(&root_path)
            .ok_or_else(|| AgentControlError::AgentNotFound(root_path.clone()))?;
        if state.agents.values().any(|agent| {
            agent.execution_marker.is_some()
                || agent.mailbox.iter().any(|message| message.trigger_turn)
        }) {
            return Ok(AgentRootContinuationOutcome::NotReady);
        }
        if !root.run_control.success_is_sealed()
            || !matches!(root.status, AgentStatus::Completed(_))
        {
            return Ok(AgentRootContinuationOutcome::Invalid);
        }
        if active_agent_count(&state) >= state.max_concurrent_agents {
            return Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: state.max_concurrent_agents,
            });
        }
        let run_control = RunControl::new();
        self.install_root_terminal_router(&run_control)?;
        let marker = Arc::new(());
        let root = state
            .agents
            .get_mut(&root_path)
            .expect("root existence was checked while holding the same registry lock");
        root.execution_marker = Some(Arc::clone(&marker));
        root.run_control = run_control.clone();
        root.status = AgentStatus::PendingInit;
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
    /// Active executions and queued trigger turns retain the capacity frozen at their admission.
    pub fn reconfigure_max_concurrent_agents(
        &self,
        max_concurrent_agents: usize,
    ) -> Result<(), AgentControlError> {
        if !(1..=MAX_RETAINED_AGENTS).contains(&max_concurrent_agents) {
            return Err(AgentControlError::InvalidCapacity {
                requested: max_concurrent_agents,
                max_retained_agents: MAX_RETAINED_AGENTS,
            });
        }
        let mut state = self.lock()?;
        if state.agents.values().any(agent_has_live_work) {
            return Err(AgentControlError::TreeNotQuiescent);
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
        status: InactiveAgentStatus,
        initial_activity: Option<String>,
    ) -> Result<AgentSnapshot, AgentControlError> {
        if !parent.is_root() {
            return Err(AgentControlError::InvalidChildParent(parent.clone()));
        }
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
        if state.agents.len() >= MAX_RETAINED_AGENTS {
            return Err(AgentControlError::AgentRegistryFull {
                max_retained_agents: MAX_RETAINED_AGENTS,
            });
        }

        let spawn_order = allocate_spawn_order(&mut state)?;
        let run_control = RunControl::new();
        let (mailbox_activity_tx, _) = watch::channel(0);
        state.agents.insert(
            child_path.clone(),
            AgentEntry {
                session_id,
                parent: Some(parent.clone()),
                spawn_order,
                status: status.into(),
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

    pub fn status(&self, path: &AgentPath) -> Result<AgentStatus, AgentControlError> {
        let state = self.lock()?;
        state
            .agents
            .get(path)
            .map(|agent| agent.status.clone())
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))
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

    /// Commits canonical communication content and only then enqueues its
    /// identity-only wake notice. There is deliberately no non-durable enqueue
    /// API: message content has exactly one owner, the canonical history stream.
    pub fn commit_and_enqueue_mail(
        &self,
        author_path: &AgentPath,
        recipient_path: &AgentPath,
        trigger_turn: bool,
        durable_commit: impl FnOnce() -> Result<HistoryItemId, String>,
    ) -> Result<AgentMailDeliveryOutcome, AgentControlError> {
        let _delivery = self.lock_mail_delivery()?;
        let (author_session_id, recipient_session_id, trigger_admission_epoch) = {
            let state = self.lock()?;
            if trigger_turn && state.root_scope_control.is_cancelled() {
                return Err(AgentControlError::TreeCancelled);
            }
            let author = state
                .agents
                .get(author_path)
                .ok_or_else(|| AgentControlError::AgentNotFound(author_path.clone()))?;
            let recipient = state
                .agents
                .get(recipient_path)
                .ok_or_else(|| AgentControlError::AgentNotFound(recipient_path.clone()))?;
            if trigger_turn && recipient.trigger_purge_pending > 0 {
                return Err(AgentControlError::MailboxClosed(recipient_path.clone()));
            }
            if recipient.mailbox.len() >= MAX_AGENT_MAILBOX_NOTICES {
                return Err(AgentControlError::MailboxFull {
                    recipient: recipient_path.clone(),
                    capacity: MAX_AGENT_MAILBOX_NOTICES,
                });
            }
            (
                author.session_id,
                recipient.session_id,
                recipient.trigger_admission_epoch,
            )
        };
        let history_item_id = durable_commit().map_err(AgentControlError::DurableMailboxCommit)?;
        let mut state = self.lock()?;
        if !state
            .agents
            .get(author_path)
            .is_some_and(|author| author.session_id == author_session_id)
        {
            return Err(AgentControlError::AgentNotFound(author_path.clone()));
        }
        let suppress_trigger = trigger_turn
            && (state.root_scope_control.is_cancelled()
                || !state.agents.get(recipient_path).is_some_and(|recipient| {
                    recipient.session_id == recipient_session_id
                        && recipient.trigger_admission_epoch == trigger_admission_epoch
                        && recipient.trigger_purge_pending == 0
                        && !matches!(recipient.status, AgentStatus::Shutdown)
                }));
        let recipient = state
            .agents
            .get_mut(recipient_path)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient_path.clone()))?;
        if recipient.session_id != recipient_session_id {
            return Err(AgentControlError::AgentNotFound(recipient_path.clone()));
        }
        if suppress_trigger {
            return Ok(AgentMailDeliveryOutcome::Suppressed);
        }
        recipient.mailbox_generation = recipient.mailbox_generation.wrapping_add(1);
        let generation = recipient.mailbox_generation;
        recipient.mailbox.push_back(AgentMailboxNotice {
            history_item_id,
            trigger_turn,
            generation,
        });
        recipient.mailbox_activity_tx.send_replace(generation);
        let scheduled = if trigger_turn {
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

    pub fn complete_execution(
        &self,
        lease: AgentExecutionLease,
        status: InactiveAgentStatus,
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
        agent.status = status.into();
        agent.last_activity = activity;
        agent.execution_marker = None;
        let scheduled = if state.root_scope_control.is_cancelled() {
            Vec::new()
        } else {
            self.reserve_pending_triggered_executions_locked(&mut state)
        };
        drop(state);
        self.notify_activity();
        drop(lease);
        Ok(scheduled)
    }

    /// Removes only an in-memory child whose durable spawn is being rolled back.
    ///
    /// Completed children are retained in this projection and remain queryable through their
    /// durable session and lineage. This is intentionally not a general registry deletion API.
    pub fn rollback_child_registration(
        &self,
        lease: &AgentExecutionLease,
        session_id: SessionId,
    ) -> Result<(), AgentControlError> {
        let path = &lease.path;
        if path.is_root() {
            return Err(AgentControlError::RootAgentCannotBeRemoved);
        }
        let _delivery = self.lock_mail_delivery()?;
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        if agent.session_id != session_id
            || !agent
                .execution_marker
                .as_ref()
                .is_some_and(|marker| Arc::ptr_eq(marker, &lease.marker))
            || !matches!(agent.status, AgentStatus::PendingInit)
            || !agent.mailbox.is_empty()
        {
            return Err(AgentControlError::AgentRollbackRejected(path.clone()));
        }
        let agent = state
            .agents
            .remove(path)
            .expect("rollback target was validated under the same registry lock");
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
            && (state.root_scope_control.is_cancelled()
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
    ) -> Result<Vec<AgentMailboxNotice>, AgentControlError> {
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

    pub fn mailbox_history_item_ids(
        &self,
        recipient: &AgentPath,
    ) -> Result<Vec<HistoryItemId>, AgentControlError> {
        let state = self.lock()?;
        let agent = state
            .agents
            .get(recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient.clone()))?;
        Ok(agent
            .mailbox
            .iter()
            .map(|notice| notice.history_item_id)
            .collect())
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
        if state.root_scope_control.cause().is_some() {
            return crate::runtime::RunCancelOutcome::Rejected;
        }
        let detached_after_root_success = root.run_control.success_is_sealed();
        let outcome = if detached_after_root_success {
            let requester_is_live_descendant = !requesting_path.is_root()
                && state
                    .agents
                    .get(requesting_path)
                    .is_some_and(agent_has_live_work);
            if !requester_is_live_descendant {
                return crate::runtime::RunCancelOutcome::Rejected;
            }
            RunControl::request_linked_cancellation(
                &state.root_scope_control,
                approval_aborted.clone(),
                requesting_control,
                approval_aborted.clone(),
            )
        } else {
            RunControl::request_linked_cancellation(
                &root.run_control,
                approval_aborted.clone(),
                requesting_control,
                approval_aborted.clone(),
            )
        };
        if outcome == crate::runtime::RunCancelOutcome::Rejected {
            return outcome;
        }

        let tree_stopped =
            crate::runtime::RunCancellationCause::Interruption(TurnInterruptionCause::TreeStopped);
        // The retained root scope and every root-turn transition are serialized by `state`. The
        // detached sealed-root branch already linked that scope with the requester above.
        let scope_owns_stop = if detached_after_root_success {
            true
        } else {
            let scope_outcome = state
                .root_scope_control
                .request_cancel_local(approval_aborted.clone());
            scope_outcome != crate::runtime::RunCancelOutcome::Rejected
                || state.root_scope_control.cause().as_ref() == Some(&approval_aborted)
        };
        debug_assert!(
            scope_owns_stop,
            "an accepted root permission Abort must own the retained root scope"
        );
        if scope_owns_stop {
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
            root.run_control.request_cancel_local(root_cause.clone()),
            crate::runtime::RunCancelOutcome::Applied
        );
        let scope_applied = matches!(
            state
                .root_scope_control
                .request_cancel_local(root_cause.clone()),
            crate::runtime::RunCancelOutcome::Applied
        );
        if !scope_applied && state.root_scope_control.cause().is_none() {
            return root_applied;
        }
        for (path, agent) in &state.agents {
            if !path.is_root() {
                agent.run_control.cancel(descendant_cause.clone());
            }
        }
        drop(state);
        self.notify_activity();
        root_applied || scope_applied
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
        let source_is_scope = root_route.is_some_and(|(source, kind)| {
            kind == RunTerminalRouteKind::Request && state.root_scope_control.same_owner(source)
        });
        let source_is_root_turn =
            root_route.is_some_and(|(source, _)| root.run_control.same_owner(source));
        if root_route.is_some() && !source_is_scope && !source_is_root_turn {
            return TreeClassificationResult::unroutable();
        }
        let mut effective_root_cause = root_cause;
        let mut effective_descendant_cause = descendant_cause;
        let mut effective_allow_detached_tree_action = allow_detached_tree_action;
        let root_turn_route_kind = source_is_root_turn.then(|| {
            root_route
                .expect("a matched root turn source must have a route")
                .1
        });
        let preclassified_root_outcome = match root_turn_route_kind {
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
        let detached_request = root_route.is_none()
            || root_route.is_some_and(|(_, kind)| kind == RunTerminalRouteKind::Request);
        if detached_request && root_success_is_durable && effective_allow_detached_tree_action {
            if state
                .root_scope_control
                .cause()
                .is_some_and(|existing| existing != effective_root_cause)
            {
                return TreeClassificationResult::rejected();
            }
            let scope_outcome = state
                .root_scope_control
                .request_cancel_local(effective_root_cause.clone());
            let scope_applied = scope_outcome == RunCancelOutcome::Applied;
            let scope_owns_requested_cause = scope_applied
                || state.root_scope_control.cause().as_ref() == Some(&effective_root_cause);
            if !scope_owns_requested_cause {
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
                root_outcome: if source_is_scope {
                    scope_outcome
                } else {
                    RunCancelOutcome::Rejected
                },
                tree_applied: scope_applied,
                source_matched: true,
            };
        }
        if state
            .root_scope_control
            .cause()
            .is_some_and(|existing| existing != effective_root_cause)
        {
            return TreeClassificationResult::rejected();
        }
        let root_outcome = preclassified_root_outcome.unwrap_or_else(|| {
            root.run_control
                .request_cancel_local(effective_root_cause.clone())
        });
        let root_owns_requested_cause =
            matches!(root_outcome, crate::runtime::RunCancelOutcome::Applied)
                || root.run_control.cause().as_ref() == Some(&effective_root_cause);
        let deferred_tree_action = match root_outcome {
            crate::runtime::RunCancelOutcome::Deferred(deferral) => {
                effective_allow_detached_tree_action || !deferral.is_success_commit_only()
            }
            crate::runtime::RunCancelOutcome::Applied
            | crate::runtime::RunCancelOutcome::Rejected => false,
        };
        if !root_owns_requested_cause && !deferred_tree_action {
            return TreeClassificationResult {
                root_outcome,
                tree_applied: false,
                source_matched: true,
            };
        }
        let scope_outcome = state
            .root_scope_control
            .request_cancel_local(effective_root_cause.clone());
        let scope_applied = scope_outcome == RunCancelOutcome::Applied;
        let scope_owns_requested_cause = scope_applied
            || state.root_scope_control.cause().as_ref() == Some(&effective_root_cause);
        if !scope_owns_requested_cause {
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
            root_outcome: if source_is_scope
                && !matches!(root_outcome, RunCancelOutcome::Deferred(_))
            {
                scope_outcome
            } else {
                root_outcome
            },
            tree_applied: scope_applied,
            source_matched: true,
        }
    }

    pub fn tree_is_cancelled(&self) -> bool {
        self.lock()
            .is_ok_and(|state| state.root_scope_control.is_cancelled())
    }

    fn mutate_execution(
        &self,
        path: &AgentPath,
        marker: &Arc<()>,
        mutation: impl FnOnce(&mut AgentEntry),
    ) -> Result<(), AgentControlError> {
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        if !agent
            .execution_marker
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, marker))
        {
            return Err(AgentControlError::StaleExecution(path.clone()));
        }
        mutation(agent);
        drop(state);
        self.notify_activity();
        Ok(())
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
            agent.status = AgentStatus::Interrupted;
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

    pub fn scope(&self) -> AgentExecutionScope {
        AgentExecutionScope {
            control: self.control.clone(),
            path: self.path.clone(),
            marker: Arc::downgrade(&self.marker),
        }
    }

    pub fn set_status(&self, status: ActiveAgentStatus) -> Result<(), AgentControlError> {
        self.control
            .mutate_execution(&self.path, &self.marker, |agent| {
                agent.status = status.into()
            })
    }

    pub fn set_activity(&self, activity: Option<String>) -> Result<(), AgentControlError> {
        self.control
            .mutate_execution(&self.path, &self.marker, |agent| {
                agent.last_activity = activity
            })
    }
}

impl AgentExecutionScope {
    pub fn path(&self) -> &AgentPath {
        &self.path
    }

    pub fn set_status(&self, status: ActiveAgentStatus) -> Result<(), AgentControlError> {
        let marker = self
            .marker
            .upgrade()
            .ok_or_else(|| AgentControlError::StaleExecution(self.path.clone()))?;
        self.control
            .mutate_execution(&self.path, &marker, |agent| agent.status = status.into())
    }

    pub fn set_activity(&self, activity: Option<String>) -> Result<(), AgentControlError> {
        let marker = self
            .marker
            .upgrade()
            .ok_or_else(|| AgentControlError::StaleExecution(self.path.clone()))?;
        self.control
            .mutate_execution(&self.path, &marker, |agent| agent.last_activity = activity)
    }

    pub fn set_status_and_activity(
        &self,
        status: ActiveAgentStatus,
        activity: Option<String>,
    ) -> Result<(), AgentControlError> {
        let marker = self
            .marker
            .upgrade()
            .ok_or_else(|| AgentControlError::StaleExecution(self.path.clone()))?;
        self.control.mutate_execution(&self.path, &marker, |agent| {
            agent.status = status.into();
            agent.last_activity = activity;
        })
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

fn allocate_spawn_order(state: &mut AgentTreeState) -> Result<u64, AgentControlError> {
    let spawn_order = state.next_spawn_order;
    state.next_spawn_order = state
        .next_spawn_order
        .checked_add(1)
        .ok_or(AgentControlError::SpawnOrderExhausted)?;
    Ok(spawn_order)
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

    fn enqueue_test_notice(
        control: &AgentControl,
        author: &AgentPath,
        recipient: &AgentPath,
        trigger_turn: bool,
    ) -> Result<(HistoryItemId, AgentMailDeliveryOutcome), AgentControlError> {
        let history_item_id = HistoryItemId::new();
        let outcome = control
            .commit_and_enqueue_mail(author, recipient, trigger_turn, || Ok(history_item_id))?;
        Ok((history_item_id, outcome))
    }

    fn admitted_continuation(
        outcome: Result<AgentRootContinuationOutcome, AgentControlError>,
    ) -> AgentExecutionLease {
        match outcome.expect("continuation outcome") {
            AgentRootContinuationOutcome::Admitted(lease) => lease,
            AgentRootContinuationOutcome::Blocked
            | AgentRootContinuationOutcome::NotReady
            | AgentRootContinuationOutcome::Invalid => panic!("continuation was not admitted"),
        }
    }

    #[test]
    fn root_scope_and_every_turn_have_distinct_owners() {
        let root_scope = RunControl::new();
        let (control, first_execution) =
            AgentControl::with_root_control(SessionId::new(), 1, root_scope.clone())
                .expect("agent tree");
        let first_turn = first_execution.run_control();

        assert!(!root_scope.same_owner(&first_turn));
        assert!(first_turn.seal_success());
        control
            .complete_execution(first_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete first turn");

        let second_execution =
            admitted_continuation(control.try_acquire_root_continuation(root_scope.clone()));
        let second_turn = second_execution.run_control();
        assert!(!second_turn.same_owner(&root_scope));
        assert!(!second_turn.same_owner(&first_turn));
        assert!(first_turn.success_is_sealed());
        assert_eq!(root_scope.cause(), None);
        assert_eq!(second_turn.cause(), None);
    }

    #[test]
    fn stop_before_continuation_blocks_admission_and_preserves_durable_success() {
        let root_scope = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_scope.clone())
                .expect("agent tree");
        let root_turn = root_execution.run_control();
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_turn = child_execution.run_control();
        let success = root_turn.begin_success_commit().expect("success commit");

        assert!(matches!(
            root_scope.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
            RunCancelOutcome::Deferred(_)
        ));
        assert_eq!(
            root_scope.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert_eq!(
            child_turn.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        assert!(success.seal());
        assert!(root_turn.success_is_sealed());
        control
            .complete_execution(root_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete durable success");
        control
            .complete_execution(child_execution, InactiveAgentStatus::Interrupted, None)
            .expect("settle child");

        assert!(matches!(
            control
                .try_acquire_root_continuation(root_scope)
                .expect("continuation outcome"),
            AgentRootContinuationOutcome::Blocked
        ));
        assert!(control.tree_is_cancelled());
    }

    #[test]
    fn continuation_admission_before_stop_cancels_the_fresh_turn() {
        let root_scope = RunControl::new();
        let (control, first_execution) =
            AgentControl::with_root_control(SessionId::new(), 1, root_scope.clone())
                .expect("agent tree");
        let first_turn = first_execution.run_control();
        assert!(first_turn.seal_success());
        control
            .complete_execution(first_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete first turn");

        let continuation =
            admitted_continuation(control.try_acquire_root_continuation(root_scope.clone()));
        let continuation_turn = continuation.run_control();
        assert!(!continuation_turn.same_owner(&first_turn));
        assert_eq!(
            root_scope.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
            RunCancelOutcome::Applied
        );
        assert!(first_turn.success_is_sealed());
        assert_eq!(
            continuation_turn.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert!(continuation.cancel_token().is_cancelled());
        assert!(control.tree_is_cancelled());
        control
            .complete_execution(continuation, InactiveAgentStatus::Interrupted, None)
            .expect("settle stopped continuation");
    }

    #[test]
    fn continuation_waits_for_durable_success_child_settlement_and_trigger_mail() {
        let root_scope = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_scope.clone())
                .expect("agent tree");
        let root_turn = root_execution.run_control();
        let (child, child_execution) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");

        assert!(matches!(
            control
                .try_acquire_root_continuation(root_scope.clone())
                .expect("active outcome"),
            AgentRootContinuationOutcome::NotReady
        ));
        assert!(root_turn.seal_success());
        control
            .complete_execution(root_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete root");
        assert!(matches!(
            control
                .try_acquire_root_continuation(root_scope.clone())
                .expect("child-active outcome"),
            AgentRootContinuationOutcome::NotReady
        ));
        control
            .complete_execution(child_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete child");
        let _ = enqueue_test_notice(&control, &AgentPath::root(), &child.path, true)
            .expect("trigger mail");
        assert!(matches!(
            control
                .try_acquire_root_continuation(root_scope.clone())
                .expect("mail-pending outcome"),
            AgentRootContinuationOutcome::NotReady
        ));
        control.drain_mailbox(&child.path).expect("drain mail");

        let continuation = admitted_continuation(control.try_acquire_root_continuation(root_scope));
        assert!(!continuation.run_control().same_owner(&root_turn));
    }

    #[test]
    fn continuation_rejects_a_stale_scope_and_a_non_success_terminal() {
        let root_scope = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 1, root_scope.clone())
                .expect("agent tree");
        let root_turn = root_execution.run_control();
        assert!(root_turn.seal_success());
        control
            .complete_execution(root_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete root");

        assert!(matches!(
            control
                .try_acquire_root_continuation(RunControl::new())
                .expect("stale scope outcome"),
            AgentRootContinuationOutcome::Invalid
        ));

        let next_scope = RunControl::new();
        let next_execution = control
            .try_acquire_root_execution(next_scope.clone())
            .expect("new top-level root turn");
        let next_turn = next_execution.run_control();
        control
            .complete_execution(
                next_execution,
                InactiveAgentStatus::Errored("provider failed".to_string()),
                None,
            )
            .expect("complete failed turn");
        assert!(matches!(
            control
                .try_acquire_root_continuation(next_scope)
                .expect("failed-turn outcome"),
            AgentRootContinuationOutcome::Invalid
        ));
        assert!(!next_turn.success_is_sealed());
    }

    #[test]
    fn stale_prior_turn_and_scope_cannot_cancel_the_current_turn() {
        let first_scope = RunControl::new();
        let (control, first_execution) =
            AgentControl::with_root_control(SessionId::new(), 1, first_scope.clone())
                .expect("agent tree");
        let first_turn = first_execution.run_control();
        assert!(first_turn.seal_success());
        control
            .complete_execution(first_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete first turn");

        let current_scope = RunControl::new();
        let current_execution = control
            .try_acquire_root_execution(current_scope.clone())
            .expect("current root turn");
        let current_turn = current_execution.run_control();

        assert!(!first_turn.fail("late failure from stale turn"));
        assert!(first_scope.fail("late failure from stale scope"));
        assert_eq!(current_scope.cause(), None);
        assert_eq!(current_turn.cause(), None);
        assert!(!control.tree_is_cancelled());
    }

    #[test]
    fn root_failure_closes_scope_and_descendants_while_child_failure_stays_local() {
        let root_scope = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 3, root_scope.clone())
                .expect("agent tree");
        let root_turn = root_execution.run_control();
        let (_, failed_child) = control
            .register_child(&AgentPath::root(), "failed_child", SessionId::new(), None)
            .expect("failed child");
        let (_, sibling) = control
            .register_child(&AgentPath::root(), "sibling", SessionId::new(), None)
            .expect("sibling");
        let failed_child_turn = failed_child.run_control();
        let sibling_turn = sibling.run_control();

        assert!(failed_child_turn.fail("child-only failure"));
        assert_eq!(root_scope.cause(), None);
        assert_eq!(root_turn.cause(), None);
        assert_eq!(sibling_turn.cause(), None);
        assert!(!control.tree_is_cancelled());

        let failure = RunCancellationCause::Failure("root failure".to_string());
        assert!(root_turn.fail("root failure"));
        assert_eq!(root_scope.cause(), Some(failure.clone()));
        assert_eq!(sibling_turn.cause(), Some(failure));
        assert!(control.tree_is_cancelled());
    }

    #[test]
    fn root_failure_during_tool_settlement_closes_scope_before_release() {
        let root_scope = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_scope.clone())
                .expect("agent tree");
        let root_turn = root_execution.run_control();
        let (_, child) = control
            .register_child(&AgentPath::root(), "child", SessionId::new(), None)
            .expect("child");
        let child_turn = child.run_control();
        let settlement = root_turn.begin_tool_settlement().expect("tool settlement");
        let failure = RunCancellationCause::Failure("settlement failed".to_string());

        assert!(matches!(
            root_turn.request_cancel(failure.clone()),
            RunCancelOutcome::Deferred(_)
        ));
        assert_eq!(root_scope.cause(), Some(failure.clone()));
        assert_eq!(child_turn.cause(), Some(failure.clone()));
        assert_eq!(root_turn.cause(), None);
        settlement.release();
        assert_eq!(root_turn.cause(), Some(failure));
    }

    #[test]
    fn one_live_root_scope_cannot_be_attached_to_two_trees() {
        let root_scope = RunControl::new();
        let (first_tree, _first_turn) =
            AgentControl::with_root_control(SessionId::new(), 1, root_scope.clone())
                .expect("first tree");
        assert!(matches!(
            AgentControl::with_root_control(SessionId::new(), 1, root_scope.clone()),
            Err(AgentControlError::RunControlOwnedByDifferentTree)
        ));
        assert!(!first_tree.tree_is_cancelled());
    }
    #[test]
    fn agent_paths_are_canonical_and_resolve_relative_or_absolute_references() {
        let worker = AgentPath::root().join("worker_1").expect("worker path");
        assert_eq!(worker.as_str(), "/root/worker_1");
        assert_eq!(worker.name(), "worker_1");
        assert_eq!(worker.parent(), Some(AgentPath::root()));
        assert_eq!(
            worker.resolve("reviewer").expect("relative path"),
            AgentPath::try_from("/root/reviewer").expect("canonical path")
        );
        assert_eq!(
            worker.resolve("/root/other").expect("absolute path"),
            AgentPath::try_from("/root/other").expect("canonical path")
        );

        assert!(AgentPath::root().join("BadName").is_err());
        assert!(AgentPath::root().join("two/parts").is_err());
        assert!(worker.join("reviewer").is_err());
        assert!(AgentPath::try_from("/other/worker").is_err());
        assert!(AgentPath::try_from("/root/worker/").is_err());
        assert!(AgentPath::try_from("/root/worker/reviewer").is_err());
        assert!(AgentPath::root().resolve("../sibling").is_err());
        assert!(worker.resolve("two/parts").is_err());
    }

    #[test]
    fn only_root_can_register_a_direct_child() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let (child, _child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("direct child");

        assert!(matches!(
            control.register_child(&child.path, "nested", SessionId::new(), None),
            Err(AgentControlError::InvalidChildParent(path)) if path == child.path
        ));
        assert!(matches!(
            control.restore_inactive_child(
                &child.path,
                "restored_nested",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            ),
            Err(AgentControlError::InvalidChildParent(path)) if path == child.path
        ));
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
        let (first, first_execution) = control
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

        first_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("status");
        first_execution
            .set_activity(Some("Reported findings".to_string()))
            .expect("activity");

        let snapshot = control.snapshot().expect("tree snapshot");
        assert_eq!(
            snapshot
                .agents
                .iter()
                .map(|agent| agent.spawn_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            snapshot.agents[0].children,
            vec![first.path.clone(), second.path]
        );
        assert!(snapshot.agents[1].children.is_empty());
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

    #[test]
    fn stale_execution_scope_cannot_overwrite_a_replacement_turn() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, first_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let stale_scope = first_execution.scope();
        first_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("first status");
        drop(first_execution);

        let replacement = control
            .try_acquire_execution(&child.path)
            .expect("replacement turn");
        replacement
            .set_status(ActiveAgentStatus::Running)
            .expect("replacement status");
        replacement
            .set_activity(Some("current turn".to_string()))
            .expect("replacement activity");

        assert!(matches!(
            stale_scope.set_status(ActiveAgentStatus::PendingInit),
            Err(AgentControlError::StaleExecution(path)) if path == child.path
        ));
        assert!(matches!(
            stale_scope.set_activity(Some("stale turn".to_string())),
            Err(AgentControlError::StaleExecution(path)) if path == child.path
        ));
        let current = control
            .list_agents(Some(&child.path))
            .expect("current child")
            .into_iter()
            .next()
            .expect("child snapshot");
        assert_eq!(current.status, AgentStatus::Running);
        assert_eq!(current.last_activity.as_deref(), Some("current turn"));
    }

    #[test]
    fn missing_status_uses_the_typed_not_found_error() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 1).expect("agent tree");
        let missing = AgentPath::root().join("missing").expect("missing path");
        assert!(matches!(
            control.status(&missing),
            Err(AgentControlError::AgentNotFound(path)) if path == missing
        ));
    }

    #[test]
    fn retained_registry_is_bounded_independently_from_execution_capacity() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 1).expect("agent tree");
        let root = AgentPath::root();
        for index in 0..(MAX_RETAINED_AGENTS - 1) {
            control
                .restore_inactive_child(
                    &root,
                    &format!("child_{index}"),
                    SessionId::new(),
                    InactiveAgentStatus::Completed(None),
                    None,
                )
                .expect("retained child within capacity");
        }
        assert_eq!(
            control.snapshot().expect("bounded snapshot").agents.len(),
            MAX_RETAINED_AGENTS
        );
        assert!(matches!(
            control.restore_inactive_child(
                &root,
                "overflow",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            ),
            Err(AgentControlError::AgentRegistryFull {
                max_retained_agents: MAX_RETAINED_AGENTS
            })
        ));
    }

    #[test]
    fn rollback_does_not_reuse_spawn_order_or_remove_completed_children() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let first_session_id = SessionId::new();
        let (first, first_execution) = control
            .register_child(&root, "first", first_session_id, None)
            .expect("first child");
        control
            .rollback_child_registration(&first_execution, first_session_id)
            .expect("uncommitted spawn rollback");
        drop(first_execution);

        let second_session_id = SessionId::new();
        let (second, second_execution) = control
            .register_child(&root, "second", second_session_id, None)
            .expect("second child");
        assert!(second.spawn_order > first.spawn_order);
        control
            .complete_execution(second_execution, InactiveAgentStatus::Completed(None), None)
            .expect("completed durable child");
        assert_eq!(
            control
                .status(&second.path)
                .expect("retained completed child"),
            AgentStatus::Completed(None)
        );
    }

    #[test]
    fn concurrency_capacity_cannot_exceed_the_retained_registry_bound() {
        for invalid in [0, MAX_RETAINED_AGENTS + 1] {
            assert!(matches!(
                AgentControl::new(SessionId::new(), invalid),
                Err(AgentControlError::InvalidCapacity {
                    requested,
                    max_retained_agents: MAX_RETAINED_AGENTS
                }) if requested == invalid
            ));
        }
    }

    #[test]
    fn child_lifecycle_cannot_publish_active_terminal_or_inactive_running_pairs() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, first_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        first_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("running child");
        control
            .complete_execution(
                first_execution,
                InactiveAgentStatus::Completed(Some("first result".to_string())),
                None,
            )
            .expect("complete first turn");

        let generation_before = control.activity_generation();
        let second_execution = control
            .try_acquire_execution(&child.path)
            .expect("follow-up turn");
        let active = control
            .list_agents(Some(&child.path))
            .expect("active child")[0]
            .clone();
        assert!(active.is_active);
        assert_eq!(active.status, AgentStatus::PendingInit);
        assert_ne!(control.activity_generation(), generation_before);

        drop(second_execution);
        let dropped = control
            .list_agents(Some(&child.path))
            .expect("dropped child")[0]
            .clone();
        assert!(!dropped.is_active);
        assert_eq!(dropped.status, AgentStatus::Interrupted);

        let shutdown_execution = control
            .try_acquire_execution(&child.path)
            .expect("shutdown turn");
        control
            .complete_execution(shutdown_execution, InactiveAgentStatus::Shutdown, None)
            .expect("shutdown child");
        assert!(matches!(
            control.try_acquire_execution(&child.path),
            Err(AgentControlError::AgentShutdown(path)) if path == child.path
        ));
    }

    #[test]
    fn active_tree_capacity_is_frozen_until_quiescence() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        assert_eq!(
            control.reconfigure_max_concurrent_agents(1),
            Err(AgentControlError::TreeNotQuiescent)
        );
        assert_eq!(
            control
                .snapshot()
                .expect("active tree snapshot")
                .max_concurrent_agents,
            2
        );

        control
            .complete_execution(root_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete root");
        control
            .reconfigure_max_concurrent_agents(1)
            .expect("next-run capacity");
        assert_eq!(
            control
                .snapshot()
                .expect("quiescent tree snapshot")
                .max_concurrent_agents,
            1
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
        let first_id = HistoryItemId::new();
        let second_id = HistoryItemId::new();

        assert_eq!(
            match control
                .commit_and_enqueue_mail(&child.path, &root, false, || Ok(first_id))
                .expect("first mail")
            {
                AgentMailDeliveryOutcome::Enqueued { generation, .. } => generation,
                AgentMailDeliveryOutcome::Suppressed => panic!("first mail suppressed"),
            },
            1
        );
        assert_eq!(
            match control
                .commit_and_enqueue_mail(&child.path, &root, true, || Ok(second_id))
                .expect("second mail")
            {
                AgentMailDeliveryOutcome::Enqueued { generation, .. } => generation,
                AgentMailDeliveryOutcome::Suppressed => panic!("second mail suppressed"),
            },
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
                .map(|notice| (notice.history_item_id, notice.generation))
                .collect::<Vec<_>>(),
            vec![(first_id, 1), (second_id, 2)]
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

        let error = match control.commit_and_enqueue_mail(&child.path, &root, false, || {
            Err("injected sqlite failure".to_string())
        }) {
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

        let history_item_id = HistoryItemId::new();
        let outcome = control
            .commit_and_enqueue_mail(&child.path, &root, false, || Ok(history_item_id))
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
                .map(|notice| notice.history_item_id)
                .collect::<Vec<_>>(),
            vec![history_item_id]
        );
    }

    #[test]
    fn durable_mailbox_is_bounded_before_content_commit() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, _child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        for _ in 0..MAX_AGENT_MAILBOX_NOTICES {
            let _ = enqueue_test_notice(&control, &child.path, &root, false)
                .expect("notice within capacity");
        }

        let commit_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let commit_called_in_closure = Arc::clone(&commit_called);
        let error = match control.commit_and_enqueue_mail(&child.path, &root, false, move || {
            commit_called_in_closure.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(HistoryItemId::new())
        }) {
            Err(error) => error,
            Ok(_) => panic!("overflow must apply backpressure before durable commit"),
        };
        assert!(matches!(
            error,
            AgentControlError::MailboxFull {
                recipient,
                capacity: MAX_AGENT_MAILBOX_NOTICES
            } if recipient == root
        ));
        assert!(!commit_called.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            control.list_agents(Some(&root)).expect("root snapshot")[0].pending_mail_count,
            MAX_AGENT_MAILBOX_NOTICES
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
            sender_control.commit_and_enqueue_mail(&child_path, &sender_root, false, || {
                commit_entered_tx.send(()).expect("commit entered signal");
                release_commit_rx.recv().expect("release durable commit");
                Ok(HistoryItemId::new())
            })
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
            sender_control.commit_and_enqueue_mail(&sender_root, &child_path, true, || {
                commit_entered_tx.send(()).expect("commit entered signal");
                release_commit_rx.recv().expect("release durable commit");
                sender_committed.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(HistoryItemId::new())
            })
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
            sender_control.commit_and_enqueue_mail(&sender_root, &sender_child, true, || {
                commit_entered_tx.send(()).expect("commit entered signal");
                release_commit_rx.recv().expect("release durable commit");
                Ok(HistoryItemId::new())
            })
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
        let _ =
            enqueue_test_notice(&control, &root, &child.path, false).expect("informational mail");
        let _ = enqueue_test_notice(&control, &root, &child.path, true).expect("trigger mail");

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
        for terminal_status in [
            InactiveAgentStatus::Completed(None),
            InactiveAgentStatus::Interrupted,
        ] {
            let (control, _root_execution) =
                AgentControl::new(SessionId::new(), 2).expect("agent tree");
            let root = AgentPath::root();
            let (child, child_execution) = control
                .register_child(&root, "worker", SessionId::new(), None)
                .expect("worker");
            let _ = enqueue_test_notice(&control, &root, &child.path, true).expect("stale trigger");

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
                AgentStatus::from(terminal_status)
            );

            let outcome = control
                .commit_and_enqueue_mail(&root, &child.path, true, || Ok(HistoryItemId::new()))
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
            replacement_entry.mailbox_generation =
                replacement_entry.mailbox_generation.wrapping_add(1);
            replacement_entry.mailbox.push_back(AgentMailboxNotice {
                history_item_id: HistoryItemId::new(),
                trigger_turn: true,
                generation: replacement_entry.mailbox_generation,
            });
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
        let _ = enqueue_test_notice(&control, &root, &child.path, true).expect("trigger mail");

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
