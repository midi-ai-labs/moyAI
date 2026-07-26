use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::protocol::{HistoryItemId, TurnId, TurnInterruptionCause};
use crate::runtime::cancel::{RunTerminalRoute, RunTerminalRouteKind};
use crate::runtime::{RunCancelOutcome, RunControl};
use crate::session::SessionId;
use crate::storage::session_repo::{MAX_DURABLE_AGENT_MAILBOX_MESSAGES, OwnerResumeRequestId};
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
        let (parent, _) = self.as_str().rsplit_once('/')?;
        Self::from_string(parent.to_string()).ok()
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
    AwaitingDescendants,
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
    /// Durable initial/follow-up input exists, but no process-local execution owns it yet.
    PendingInit,
    /// This agent has a durable deferred terminal owned by unsettled descendants.
    AwaitingDescendants(TurnId),
    Interrupted,
    Completed(Option<String>),
    Errored(String),
    Shutdown,
}

impl From<InactiveAgentStatus> for AgentStatus {
    fn from(status: InactiveAgentStatus) -> Self {
        match status {
            InactiveAgentStatus::PendingInit => Self::PendingInit,
            InactiveAgentStatus::AwaitingDescendants(_) => Self::AwaitingDescendants,
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
    /// True when this durable item is session-scoped and must eventually admit a new turn.
    ///
    /// A trigger delivered into an already-running turn remains canonical input, but it does not
    /// schedule a duplicate continuation after that turn completes. A deferred owner may retain a
    /// true trigger while storage temporarily withholds immediate scheduling until descendant
    /// settlement supplies the canonical wake.
    pub trigger_turn: bool,
    /// True when durable storage currently authorizes this trigger to reserve an execution lease.
    ///
    /// A completed-early owner keeps a dormant trigger until descendant settlement promotes it.
    /// Turn-scoped input stays dormant because its current admission already owns the content.
    pub(crate) schedule_ready: bool,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMailCommit {
    pub history_item_id: HistoryItemId,
    /// Whether storage authorizes an immediate execution reservation for this durable item.
    pub schedule_turn: bool,
    pub(crate) owner_resume_request_id: Option<OwnerResumeRequestId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMailboxDeliveryCommit {
    pub history_item_ids: Vec<HistoryItemId>,
    pub has_more: bool,
}

const MAX_AGENT_MAILBOX_NOTICES: usize = MAX_DURABLE_AGENT_MAILBOX_MESSAGES;

/// Root plus all descendants retained by one process-local tree.
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
    #[error("agent spawn order {0} is already retained or reserved for root")]
    SpawnOrderAlreadyUsed(u64),
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
    #[error("durable owner-resume read failed: {0}")]
    DurableOwnerResumeRead(String),
    #[error("durable agent admission commit failed: {0}")]
    DurableAdmissionCommit(String),
    #[error("durable agent interruption commit failed: {0}")]
    DurableInterruptCommit(String),
    #[error("durable agent spawn commit failed: {0}")]
    DurableSpawnCommit(String),
    #[error("agent control lock was poisoned")]
    LockPoisoned,
    #[error("agent `{0}` execution lease is stale")]
    StaleExecution(AgentPath),
    #[error("agent `{0}` is not awaiting descendant settlement")]
    AgentNotAwaitingDescendants(AgentPath),
    #[error("the root agent cannot be removed from its tree")]
    RootAgentCannotBeRemoved,
    #[error("agent `{0}` is not an uncommitted child registration")]
    AgentRollbackRejected(AgentPath),
    #[error("the root run control is already owned by a different live agent tree")]
    RunControlOwnedByDifferentTree,
    #[error("root turns must be acquired through a retained root scope")]
    RootTurnRequiresScope,
}

#[derive(Clone)]
pub struct AgentControl {
    inner: Arc<AgentControlInner>,
}

struct AgentControlInner {
    root_terminal_router: Arc<RunTerminalRoute>,
    spawn_tree_fence: Mutex<()>,
    state: Mutex<AgentTreeState>,
    mail_delivery: Mutex<()>,
    activity_tx: watch::Sender<u64>,
}

#[derive(Clone, Copy, Debug)]
struct TreeClassificationResult {
    root_outcome: RunCancelOutcome,
    tree_applied: bool,
}

impl TreeClassificationResult {
    fn rejected() -> Self {
        Self {
            root_outcome: RunCancelOutcome::Rejected,
            tree_applied: false,
        }
    }

    fn changed(self) -> bool {
        matches!(self.root_outcome, RunCancelOutcome::Applied) || self.tree_applied
    }
}

struct AgentTreeState {
    max_concurrent_agents: usize,
    next_spawn_order: u64,
    pending_capacity_reservations: usize,
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
    pending_owner_resume_request_id: Option<OwnerResumeRequestId>,
    active_durable_turn_id: Option<TurnId>,
    awaiting_deferred_turn_id: Option<TurnId>,
    pending_deferred_release: Option<TurnId>,
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
    wake_cause: Option<AgentExecutionWakeCause>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentExecutionWakeCause {
    ExplicitTask(HistoryItemId),
    OwnerResume(OwnerResumeRequestId),
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

/// Opaque identity for one observed target execution.
///
/// A path alone is reusable. Retaining the marker and session identity lets an interrupt reject
/// when the observed turn finishes and a later turn starts at the same path.
#[derive(Clone)]
pub(crate) struct AgentInterruptTarget {
    path: AgentPath,
    session_id: SessionId,
    status: AgentStatus,
    marker: Option<Arc<()>>,
    run_control: RunControl,
}

impl AgentInterruptTarget {
    pub(crate) fn path(&self) -> &AgentPath {
        &self.path
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn status(&self) -> &AgentStatus {
        &self.status
    }
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
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PendingTriggerTerminalCommit<T> {
    Applied(T),
    BlockedByPendingDeferredCompletion { deferred_turn_id: TurnId },
    WakeOwnedOrResolved,
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
    /// `root_scope_control` retains the task-lifecycle identity and explicit tree-stop fence.
    /// Its terminal router sends an ordinary surface Stop or failure to the exact current root
    /// execution; only [`Self::interrupt_tree`] cascades through the scope and descendants. The
    /// returned execution lease always owns a fresh, turn-scoped [`RunControl`].
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
                pending_owner_resume_request_id: None,
                active_durable_turn_id: None,
                awaiting_deferred_turn_id: None,
                pending_deferred_release: None,
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
                spawn_tree_fence: Mutex::new(()),
                state: Mutex::new(AgentTreeState {
                    max_concurrent_agents,
                    next_spawn_order: 1,
                    pending_capacity_reservations: 0,
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
            wake_cause: None,
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
        self.register_child_with_order(parent, task_name, session_id, initial_activity, None)
    }

    pub(crate) fn register_child_with_order(
        &self,
        parent: &AgentPath,
        task_name: &str,
        session_id: SessionId,
        initial_activity: Option<String>,
        durable_spawn_order: Option<u64>,
    ) -> Result<(AgentSnapshot, AgentExecutionLease), AgentControlError> {
        let child_path = parent
            .join(task_name)
            .map_err(AgentControlError::InvalidPath)?;
        let mut state = self.lock()?;
        validate_child_registration_locked(&state, parent, &child_path, session_id)?;
        let (snapshot, marker, run_control) = insert_child_locked(
            &mut state,
            parent,
            child_path.clone(),
            session_id,
            initial_activity,
            durable_spawn_order,
        )?;
        drop(state);
        self.notify_activity();

        Ok((
            snapshot,
            AgentExecutionLease {
                control: self.clone(),
                path: child_path,
                marker,
                run_control,
                wake_cause: None,
            },
        ))
    }

    /// Serializes a caller-owned durable spawn with tree-wide Stop.
    ///
    /// The exact execution scope is checked while both the tree mutation fence and registry are
    /// held. The durable commit then runs before the child becomes visible in memory. Therefore a
    /// Stop that wins the fence prevents the closure from running, while a spawn that wins makes
    /// the registered child part of the Stop snapshot.
    pub(crate) fn commit_spawn<T>(
        &self,
        caller: &AgentExecutionScope,
        parent: &AgentPath,
        task_name: &str,
        session_id: SessionId,
        initial_activity: Option<String>,
        durable_commit: impl FnOnce() -> Result<(T, u64), String>,
    ) -> Result<(T, AgentSnapshot, AgentExecutionLease), AgentControlError> {
        let child_path = parent
            .join(task_name)
            .map_err(AgentControlError::InvalidPath)?;
        let _spawn_tree_fence = self.lock_spawn_tree_fence()?;
        let mut state = self.lock()?;
        validate_execution_scope_locked(self, &state, caller, parent)?;
        validate_child_registration_locked(&state, parent, &child_path, session_id)?;

        let (durable, durable_spawn_order) =
            durable_commit().map_err(AgentControlError::DurableSpawnCommit)?;
        let (snapshot, marker, run_control) = insert_child_locked(
            &mut state,
            parent,
            child_path.clone(),
            session_id,
            initial_activity,
            Some(durable_spawn_order),
        )?;
        drop(state);
        self.notify_activity();
        Ok((
            durable,
            snapshot,
            AgentExecutionLease {
                control: self.clone(),
                path: child_path,
                marker,
                run_control,
                wake_cause: None,
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
        if active_agent_count(&state) >= descendant_capacity(&state) {
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
        agent.active_durable_turn_id = None;
        agent.awaiting_deferred_turn_id = None;
        agent.pending_deferred_release = None;
        drop(state);
        self.notify_activity();

        Ok(AgentExecutionLease {
            control: self.clone(),
            path: path.clone(),
            marker,
            run_control,
            wake_cause: None,
        })
    }

    /// Starts a new user-requested root task on a retained agent tree.
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
        let root = state
            .agents
            .get(&root_path)
            .expect("an agent tree must retain its root");
        if root.execution_marker.is_some() {
            return Err(AgentControlError::AgentAlreadyActive(root_path));
        }
        if root_scope_control.is_cancelled() || root_scope_control.success_is_sealed() {
            return Err(AgentControlError::TreeCancelled);
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
        root.active_durable_turn_id = None;
        root.awaiting_deferred_turn_id = None;
        root.pending_deferred_release = None;
        drop(state);
        self.notify_activity();

        Ok(AgentExecutionLease {
            control: self.clone(),
            path: root_path,
            marker,
            run_control: root_turn_control,
            wake_cause: None,
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
        if !root.run_control.success_is_sealed()
            || !matches!(root.status, AgentStatus::Completed(_))
        {
            return Ok(AgentRootContinuationOutcome::Invalid);
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
        root.active_durable_turn_id = None;
        root.awaiting_deferred_turn_id = None;
        root.pending_deferred_release = None;
        drop(state);
        self.notify_activity();
        Ok(AgentRootContinuationOutcome::Admitted(
            AgentExecutionLease {
                control: self.clone(),
                path: root_path,
                marker,
                run_control,
                wake_cause: None,
            },
        ))
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
        self.restore_inactive_child_with_order(
            parent,
            task_name,
            session_id,
            status,
            initial_activity,
            None,
        )
    }

    pub(crate) fn restore_inactive_child_with_order(
        &self,
        parent: &AgentPath,
        task_name: &str,
        session_id: SessionId,
        status: InactiveAgentStatus,
        initial_activity: Option<String>,
        durable_spawn_order: Option<u64>,
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
        if state.agents.len() >= MAX_RETAINED_AGENTS {
            return Err(AgentControlError::AgentRegistryFull {
                max_retained_agents: MAX_RETAINED_AGENTS,
            });
        }

        let spawn_order = match durable_spawn_order {
            Some(order)
                if order == 0
                    || state
                        .agents
                        .values()
                        .any(|agent| agent.spawn_order == order) =>
            {
                return Err(AgentControlError::SpawnOrderAlreadyUsed(order));
            }
            Some(order) => {
                state.next_spawn_order = state.next_spawn_order.max(
                    order
                        .checked_add(1)
                        .ok_or(AgentControlError::SpawnOrderExhausted)?,
                );
                order
            }
            None => allocate_spawn_order(&mut state)?,
        };
        let run_control = RunControl::new();
        let (mailbox_activity_tx, _) = watch::channel(0);
        let awaiting_deferred_turn_id = match &status {
            InactiveAgentStatus::AwaitingDescendants(turn_id) => Some(*turn_id),
            _ => None,
        };
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
                pending_owner_resume_request_id: None,
                active_durable_turn_id: None,
                awaiting_deferred_turn_id,
                pending_deferred_release: None,
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

    /// Rehydrates an identity-only wake notice derived from canonical history.
    ///
    /// The durable query has already proved that no later turn-scoped append claimed this
    /// session-scoped trigger. Content is deliberately not duplicated into the registry.
    pub(crate) fn restore_pending_mail(
        &self,
        recipient: &AgentPath,
        history_item_id: HistoryItemId,
        schedule_ready: bool,
    ) -> Result<(), AgentControlError> {
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient.clone()))?;
        if agent
            .mailbox
            .iter()
            .any(|notice| notice.history_item_id == history_item_id)
        {
            return Ok(());
        }
        if agent.mailbox.len() >= MAX_AGENT_MAILBOX_NOTICES {
            return Err(AgentControlError::MailboxFull {
                recipient: recipient.clone(),
                capacity: MAX_AGENT_MAILBOX_NOTICES,
            });
        }
        agent.mailbox_generation = agent.mailbox_generation.wrapping_add(1);
        let generation = agent.mailbox_generation;
        agent.mailbox.push_back(AgentMailboxNotice {
            history_item_id,
            trigger_turn: true,
            schedule_ready,
            generation,
        });
        agent.mailbox_activity_tx.send_replace(generation);
        drop(state);
        self.notify_activity();
        Ok(())
    }

    /// Rehydrates a durable owner-continuation wake without manufacturing mailbox content.
    pub(crate) fn restore_pending_owner_resume(
        &self,
        owner: &AgentPath,
        request_id: OwnerResumeRequestId,
    ) -> Result<(), AgentControlError> {
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(owner)
            .ok_or_else(|| AgentControlError::AgentNotFound(owner.clone()))?;
        match agent.pending_owner_resume_request_id {
            Some(existing) if existing != request_id => {
                return Err(AgentControlError::StaleExecution(owner.clone()));
            }
            Some(_) => return Ok(()),
            None => {
                agent.pending_owner_resume_request_id = Some(request_id);
            }
        }
        drop(state);
        self.notify_activity();
        Ok(())
    }

    /// Reads and projects the current durable OwnerResume identity under the delivery fence.
    ///
    /// Runtime callers must use this boundary instead of carrying an `Option<RequestId>` across
    /// the storage/local-state boundary. Rehydration may use `restore_pending_owner_resume` while
    /// its tree is not yet published.
    pub(crate) fn restore_current_owner_resume(
        &self,
        owner: &AgentPath,
        durable_read: impl FnOnce() -> Result<Option<OwnerResumeRequestId>, String>,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        let _delivery = self.lock_mail_delivery()?;
        let request_id = durable_read().map_err(AgentControlError::DurableOwnerResumeRead)?;
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(owner)
            .ok_or_else(|| AgentControlError::AgentNotFound(owner.clone()))?;
        let owner_resume_schedulable = reconcile_current_owner_resume_request(agent, request_id);
        let scheduled = if owner_resume_schedulable && !state.root_scope_control.is_cancelled() {
            self.reserve_pending_triggered_executions_locked(&mut state)
        } else {
            Vec::new()
        };
        drop(state);
        self.notify_activity();
        Ok(scheduled)
    }

    /// Restores a live child-result wake after notice projection backpressure.
    ///
    /// Storage may coalesce OwnerResume when an explicit trigger already exists, so the request
    /// identity is optional. Dormant explicit triggers are promoted and scheduled atomically.
    pub(crate) fn restore_released_owner_wake(
        &self,
        owner: &AgentPath,
        released_owner_deferred_turn_id: Option<TurnId>,
        durable_read: impl FnOnce() -> Result<Option<OwnerResumeRequestId>, String>,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        let _delivery = self.lock_mail_delivery()?;
        let request_id = durable_read().map_err(AgentControlError::DurableOwnerResumeRead)?;
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(owner)
            .ok_or_else(|| AgentControlError::AgentNotFound(owner.clone()))?;
        let promotes_pending_triggers =
            project_deferred_owner_release(agent, released_owner_deferred_turn_id);
        let owner_resume_schedulable = reconcile_current_owner_resume_request(agent, request_id);
        if promotes_pending_triggers {
            for notice in &mut agent.mailbox {
                if notice.trigger_turn {
                    notice.schedule_ready = true;
                }
            }
        }
        let scheduled = if (!promotes_pending_triggers && !owner_resume_schedulable)
            || state.root_scope_control.is_cancelled()
        {
            Vec::new()
        } else {
            self.reserve_pending_triggered_executions_locked(&mut state)
        };
        drop(state);
        self.notify_activity();
        Ok(scheduled)
    }

    /// Publishes the durable admission owned by one exact wake and then reserves newly eligible
    /// descendants. OwnerResume is scheduler-only state, so claiming it never creates or drains a
    /// model-visible mailbox notice.
    pub(crate) fn mark_execution_admitted(
        &self,
        scope: &AgentExecutionScope,
        wake_cause: AgentExecutionWakeCause,
        turn_id: TurnId,
        activity: Option<String>,
        durable_read: impl FnOnce() -> Result<Option<OwnerResumeRequestId>, String>,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        let _delivery = self.lock_mail_delivery()?;
        let current_owner_resume_request_id =
            durable_read().map_err(AgentControlError::DurableOwnerResumeRead)?;
        let marker = scope
            .marker
            .upgrade()
            .ok_or_else(|| AgentControlError::StaleExecution(scope.path.clone()))?;
        let mut state = self.lock()?;
        validate_execution_scope_locked(self, &state, scope, &scope.path)?;
        let agent = state
            .agents
            .get_mut(&scope.path)
            .expect("execution scope validation proved agent existence");
        if !agent
            .execution_marker
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &marker))
        {
            return Err(AgentControlError::StaleExecution(scope.path.clone()));
        }
        // Durable admission has already claimed its exact wake. Reconcile to the scheduler's
        // current post-admission owner under the delivery fence: it may be None or a newly-created
        // R2, but must never be the captured pre-admission R1 by assumption.
        let _ = wake_cause;
        agent.pending_owner_resume_request_id = current_owner_resume_request_id;
        agent.status = AgentStatus::Running;
        agent.last_activity = activity;
        agent.active_durable_turn_id = Some(turn_id);
        agent.awaiting_deferred_turn_id = None;
        agent.pending_deferred_release = None;
        let scheduled = if state.root_scope_control.is_cancelled() {
            Vec::new()
        } else {
            self.reserve_pending_triggered_executions_locked(&mut state)
        };
        drop(state);
        self.notify_activity();
        Ok(scheduled)
    }

    pub(crate) fn schedule_pending_triggered_executions(
        &self,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        let mut state = self.lock()?;
        let scheduled = if state.root_scope_control.is_cancelled() {
            Vec::new()
        } else {
            self.reserve_pending_triggered_executions_locked(&mut state)
        };
        drop(state);
        if !scheduled.is_empty() {
            self.notify_activity();
        }
        Ok(scheduled)
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
        let active_agent_count = agents
            .iter()
            .filter(|agent| !agent.path.is_root() && agent.is_active)
            .count();
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
        durable_commit: impl FnOnce() -> Result<AgentMailCommit, String>,
    ) -> Result<AgentMailDeliveryOutcome, AgentControlError> {
        self.commit_and_enqueue_mail_internal(
            author_path,
            recipient_path,
            trigger_turn,
            None,
            false,
            None,
            |_| durable_commit().map_err(AgentControlError::DurableMailboxCommit),
        )
    }

    /// Capacity-aware durable delivery used by explicit task input.
    ///
    /// The callback must reject before append when it derives a ready turn but receives `false`.
    /// Dormant deferred mail may still commit and remains fenced by its notice readiness.
    pub(crate) fn commit_and_enqueue_mail_with_capacity(
        &self,
        author_execution: &AgentExecutionScope,
        author_path: &AgentPath,
        recipient_path: &AgentPath,
        trigger_turn: bool,
        durable_commit: impl FnOnce(bool) -> Result<AgentMailCommit, AgentControlError>,
    ) -> Result<AgentMailDeliveryOutcome, AgentControlError> {
        self.commit_and_enqueue_mail_internal(
            author_path,
            recipient_path,
            trigger_turn,
            None,
            false,
            Some(author_execution),
            durable_commit,
        )
    }

    /// Projects a durable child result that released or superseded an awaiting owner.
    ///
    /// Any queued explicit trigger becomes ready under the same mail/state fence and therefore
    /// takes precedence over a coalesced OwnerResume.
    pub(crate) fn commit_and_enqueue_completion_handoff(
        &self,
        author_path: &AgentPath,
        recipient_path: &AgentPath,
        released_owner_deferred_turn_id: Option<TurnId>,
        durable_commit: impl FnOnce() -> Result<AgentMailCommit, String>,
    ) -> Result<AgentMailDeliveryOutcome, AgentControlError> {
        self.commit_and_enqueue_mail_internal(
            author_path,
            recipient_path,
            false,
            released_owner_deferred_turn_id,
            true,
            None,
            |_| durable_commit().map_err(AgentControlError::DurableMailboxCommit),
        )
    }

    fn commit_and_enqueue_mail_internal(
        &self,
        author_path: &AgentPath,
        recipient_path: &AgentPath,
        trigger_turn: bool,
        released_owner_deferred_turn_id: Option<TurnId>,
        completion_handoff: bool,
        author_execution: Option<&AgentExecutionScope>,
        durable_commit: impl FnOnce(bool) -> Result<AgentMailCommit, AgentControlError>,
    ) -> Result<AgentMailDeliveryOutcome, AgentControlError> {
        let _delivery = self.lock_mail_delivery()?;
        let (
            recipient_session_id,
            ready_turn_capacity_granted,
            capacity_reserved,
            committed_generation,
        ) = {
            let mut state = self.lock()?;
            if let Some(author_execution) = author_execution {
                validate_execution_scope_locked(self, &state, author_execution, author_path)?;
            }
            if trigger_turn && state.root_scope_control.is_cancelled() {
                return Err(AgentControlError::TreeCancelled);
            }
            if !state.agents.contains_key(author_path) {
                return Err(AgentControlError::AgentNotFound(author_path.clone()));
            }
            let recipient = state
                .agents
                .get(recipient_path)
                .ok_or_else(|| AgentControlError::AgentNotFound(recipient_path.clone()))?;
            if trigger_turn && recipient.trigger_purge_pending > 0 {
                return Err(AgentControlError::MailboxClosed(recipient_path.clone()));
            }
            if trigger_turn && matches!(recipient.status, AgentStatus::Shutdown) {
                return Err(AgentControlError::AgentShutdown(recipient_path.clone()));
            }
            if !completion_handoff && recipient.mailbox.len() >= MAX_AGENT_MAILBOX_NOTICES {
                return Err(AgentControlError::MailboxFull {
                    recipient: recipient_path.clone(),
                    capacity: MAX_AGENT_MAILBOX_NOTICES,
                });
            }
            let recipient_session_id = recipient.session_id;
            let committed_generation = recipient.mailbox_generation.wrapping_add(1);
            let requires_new_slot = trigger_turn && recipient.execution_marker.is_none();
            let ready_turn_capacity_granted =
                !requires_new_slot || active_agent_count(&state) < descendant_capacity(&state);
            let capacity_reserved = requires_new_slot && ready_turn_capacity_granted;
            if capacity_reserved {
                state.pending_capacity_reservations =
                    state.pending_capacity_reservations.saturating_add(1);
            }
            (
                recipient_session_id,
                ready_turn_capacity_granted,
                capacity_reserved,
                committed_generation,
            )
        };
        let committed = match durable_commit(ready_turn_capacity_granted) {
            Ok(committed) => committed,
            Err(error) => {
                if capacity_reserved {
                    let mut state = self.lock()?;
                    state.pending_capacity_reservations =
                        state.pending_capacity_reservations.saturating_sub(1);
                }
                return Err(error);
            }
        };
        let AgentMailCommit {
            history_item_id,
            schedule_turn,
            owner_resume_request_id,
        } = committed;
        // Durable enqueue is already authoritative. Recover a poisoned live
        // projection lock and never turn a committed mailbox item into a
        // caller-visible failure that could provoke a duplicate resend.
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if capacity_reserved {
            state.pending_capacity_reservations =
                state.pending_capacity_reservations.saturating_sub(1);
        }
        debug_assert!(
            !schedule_turn || ready_turn_capacity_granted,
            "durable storage must reject a ready trigger before append when capacity was denied"
        );
        let Some(recipient) = state
            .agents
            .get_mut(recipient_path)
            .filter(|recipient| recipient.session_id == recipient_session_id)
        else {
            drop(state);
            self.notify_activity();
            return Ok(AgentMailDeliveryOutcome::Enqueued {
                generation: committed_generation,
                scheduled: Vec::new(),
            });
        };
        let promote_explicit_triggers = completion_handoff
            && project_deferred_owner_release(recipient, released_owner_deferred_turn_id);
        let notice_exists = recipient
            .mailbox
            .iter()
            .any(|notice| notice.history_item_id == history_item_id);
        if !notice_exists && recipient.mailbox.len() < MAX_AGENT_MAILBOX_NOTICES {
            recipient.mailbox_generation = recipient.mailbox_generation.wrapping_add(1);
            let generation = recipient.mailbox_generation;
            recipient.mailbox.push_back(AgentMailboxNotice {
                history_item_id,
                trigger_turn,
                schedule_ready: schedule_turn,
                generation,
            });
            recipient.mailbox_activity_tx.send_replace(generation);
        }
        let generation = recipient.mailbox_generation;
        if promote_explicit_triggers {
            for notice in &mut recipient.mailbox {
                if notice.trigger_turn {
                    notice.schedule_ready = true;
                }
            }
        }
        // OwnerResume is an independent durable scheduler projection. Its identity never proves a
        // deferred generation was released, and therefore cannot make dormant explicit mail ready.
        let owner_resume_schedulable = if completion_handoff {
            reconcile_current_owner_resume_request(recipient, owner_resume_request_id)
        } else {
            project_owner_resume_request(recipient, owner_resume_request_id)
        };
        let scheduled = if (schedule_turn || owner_resume_schedulable || promote_explicit_triggers)
            && !state.root_scope_control.is_cancelled()
        {
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

    /// Commits pending durable mailbox payloads into one exact admitted turn,
    /// then removes only the corresponding process-local wake hints.
    ///
    /// The registry lock is intentionally held through the short storage
    /// transaction. This gives delivery and exact interruption one ordering:
    /// an interrupt cannot win after scope validation but before the durable
    /// mailbox projection.
    pub(crate) fn commit_pending_mailbox_delivery(
        &self,
        execution: &AgentExecutionScope,
        durable_commit: impl FnOnce() -> Result<AgentMailboxDeliveryCommit, String>,
    ) -> Result<AgentMailboxDeliveryCommit, AgentControlError> {
        let _delivery = self.lock_mail_delivery()?;
        let mut state = self.lock()?;
        validate_execution_scope_locked(self, &state, execution, execution.path())?;
        let committed = durable_commit().map_err(AgentControlError::DurableMailboxCommit)?;
        let delivered = committed
            .history_item_ids
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if delivered.len() != committed.history_item_ids.len() {
            return Err(AgentControlError::DurableMailboxCommit(
                "durable mailbox delivery returned duplicate message identities".to_string(),
            ));
        }
        let agent = state
            .agents
            .get_mut(execution.path())
            .expect("execution scope validation proved agent existence");
        let pending_before = agent.mailbox.len();
        agent
            .mailbox
            .retain(|notice| !delivered.contains(&notice.history_item_id));
        if agent.mailbox.len() != pending_before {
            agent.mailbox_generation = agent.mailbox_generation.wrapping_add(1);
            agent
                .mailbox_activity_tx
                .send_replace(agent.mailbox_generation);
        }
        drop(state);
        if !committed.history_item_ids.is_empty() {
            self.notify_activity();
        }
        Ok(committed)
    }

    /// Commits a pre-admission child terminal while holding the same delivery fence used by
    /// inter-agent mail, then removes exactly the trigger notices made obsolete by that terminal.
    ///
    /// A deferred-owner blocker is projected back to AwaitingDescendants while this delivery fence
    /// remains held, preserving the exact trigger notice as dormant canonical input.
    pub(crate) fn commit_pending_trigger_terminal<T>(
        &self,
        lease: &AgentExecutionLease,
        blocked_activity: Option<String>,
        durable_commit: impl FnOnce() -> Result<PendingTriggerTerminalCommit<T>, String>,
    ) -> Result<PendingTriggerTerminalCommit<T>, AgentControlError> {
        let path = &lease.path;
        if path.is_root() {
            return Err(AgentControlError::AgentNotFound(path.clone()));
        }
        let _delivery = self.lock_mail_delivery()?;
        let session_id = {
            let state = self.lock()?;
            let agent = state
                .agents
                .get(path)
                .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
            if !agent
                .execution_marker
                .as_ref()
                .is_some_and(|marker| Arc::ptr_eq(marker, &lease.marker))
            {
                return Err(AgentControlError::StaleExecution(path.clone()));
            }
            agent.session_id
        };
        let (committed, blocked_deferred_turn_id) =
            match durable_commit().map_err(AgentControlError::DurableMailboxCommit)? {
                PendingTriggerTerminalCommit::Applied(committed) => (Some(committed), None),
                PendingTriggerTerminalCommit::BlockedByPendingDeferredCompletion {
                    deferred_turn_id,
                } => (None, Some(deferred_turn_id)),
                PendingTriggerTerminalCommit::WakeOwnedOrResolved => {
                    return Ok(PendingTriggerTerminalCommit::WakeOwnedOrResolved);
                }
            };
        let expected_trigger = match lease.wake_cause {
            Some(AgentExecutionWakeCause::ExplicitTask(history_item_id)) => Some(history_item_id),
            Some(AgentExecutionWakeCause::OwnerResume(_)) => None,
            None => return Err(AgentControlError::StaleExecution(path.clone())),
        };
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        if agent.session_id != session_id {
            return Err(AgentControlError::AgentNotFound(path.clone()));
        }
        if !agent
            .execution_marker
            .as_ref()
            .is_some_and(|marker| Arc::ptr_eq(marker, &lease.marker))
        {
            return Err(AgentControlError::StaleExecution(path.clone()));
        }
        if let Some(deferred_turn_id) = blocked_deferred_turn_id {
            let Some(expected_trigger) = expected_trigger else {
                return Err(AgentControlError::StaleExecution(path.clone()));
            };
            if !agent
                .mailbox
                .iter()
                .any(|notice| notice.trigger_turn && notice.history_item_id == expected_trigger)
            {
                return Err(AgentControlError::StaleExecution(path.clone()));
            }
            agent.run_control.supersede();
            agent.run_control = RunControl::new();
            for notice in &mut agent.mailbox {
                if notice.trigger_turn {
                    notice.schedule_ready = false;
                }
            }
            agent.status = AgentStatus::AwaitingDescendants;
            agent.last_activity = blocked_activity;
            agent.execution_marker = None;
            agent.active_durable_turn_id = None;
            agent.awaiting_deferred_turn_id = Some(deferred_turn_id);
            agent.pending_deferred_release = None;
            drop(state);
            self.notify_activity();
            return Ok(
                PendingTriggerTerminalCommit::BlockedByPendingDeferredCompletion {
                    deferred_turn_id,
                },
            );
        }
        let committed = committed.expect("applied terminal disposition must retain its payload");
        agent.run_control.supersede();
        match lease.wake_cause {
            Some(AgentExecutionWakeCause::ExplicitTask(expected_trigger)) => {
                let pending_before = agent.mailbox.len();
                agent.mailbox.retain(|notice| {
                    !(notice.trigger_turn && notice.history_item_id == expected_trigger)
                });
                if agent.mailbox.len() != pending_before {
                    agent.mailbox_generation = agent.mailbox_generation.wrapping_add(1);
                    agent
                        .mailbox_activity_tx
                        .send_replace(agent.mailbox_generation);
                }
            }
            Some(AgentExecutionWakeCause::OwnerResume(request_id)) => {
                if agent.pending_owner_resume_request_id == Some(request_id) {
                    agent.pending_owner_resume_request_id = None;
                }
            }
            None => return Err(AgentControlError::StaleExecution(path.clone())),
        }
        drop(state);
        self.notify_activity();
        Ok(PendingTriggerTerminalCommit::Applied(committed))
    }

    /// Retires a synthetic pre-admission execution after its durable wake was already owned or
    /// resolved.
    ///
    /// Only that trigger identity is stale. Informational mail and later trigger identities remain
    /// canonical input, and a later trigger may reserve the next execution immediately.
    pub(crate) fn retire_resolved_wake_execution(
        &self,
        lease: AgentExecutionLease,
        activity: Option<String>,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        let path = lease.path.clone();
        if path.is_root() {
            return Err(AgentControlError::AgentNotFound(path));
        }
        let expected_wake = lease
            .wake_cause
            .ok_or_else(|| AgentControlError::StaleExecution(path.clone()))?;
        let _delivery = self.lock_mail_delivery()?;
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(&path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        if !agent
            .execution_marker
            .as_ref()
            .is_some_and(|marker| Arc::ptr_eq(marker, &lease.marker))
        {
            return Err(AgentControlError::StaleExecution(path));
        }

        agent.run_control.supersede();
        match expected_wake {
            AgentExecutionWakeCause::ExplicitTask(expected_trigger) => {
                let pending_before = agent.mailbox.len();
                agent.mailbox.retain(|notice| {
                    !(notice.trigger_turn && notice.history_item_id == expected_trigger)
                });
                if agent.mailbox.len() != pending_before {
                    agent.mailbox_generation = agent.mailbox_generation.wrapping_add(1);
                    agent
                        .mailbox_activity_tx
                        .send_replace(agent.mailbox_generation);
                }
            }
            AgentExecutionWakeCause::OwnerResume(expected_request) => {
                if agent.pending_owner_resume_request_id == Some(expected_request) {
                    agent.pending_owner_resume_request_id = None;
                }
            }
        }
        agent.status = AgentStatus::Completed(None);
        agent.last_activity = activity;
        agent.execution_marker = None;
        agent.active_durable_turn_id = None;
        agent.awaiting_deferred_turn_id = None;
        agent.pending_deferred_release = None;
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

    /// Releases a synthetic pre-admission execution whose durable settlement could not be
    /// confirmed.
    ///
    /// Storage remains the owner of the trigger. No mailbox identity is discarded and no retry is
    /// launched inline, avoiding a tight failure loop. A later scheduler pass or mail delivery can
    /// reserve the retained trigger again.
    pub(crate) fn release_unsettled_trigger_execution(
        &self,
        lease: AgentExecutionLease,
        activity: Option<String>,
    ) -> Result<(), AgentControlError> {
        let path = lease.path.clone();
        if path.is_root() {
            return Err(AgentControlError::AgentNotFound(path));
        }
        let _delivery = self.lock_mail_delivery()?;
        let mut state = self.lock()?;
        let agent = state
            .agents
            .get_mut(&path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        if !agent
            .execution_marker
            .as_ref()
            .is_some_and(|marker| Arc::ptr_eq(marker, &lease.marker))
        {
            return Err(AgentControlError::StaleExecution(path));
        }

        agent.run_control.supersede();
        agent.status = AgentStatus::PendingInit;
        agent.last_activity = activity;
        agent.execution_marker = None;
        agent.active_durable_turn_id = None;
        agent.awaiting_deferred_turn_id = None;
        agent.pending_deferred_release = None;
        drop(state);
        self.notify_activity();
        drop(lease);
        Ok(())
    }

    pub fn complete_execution(
        &self,
        lease: AgentExecutionLease,
        status: InactiveAgentStatus,
        activity: Option<String>,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        let mut promotes_pending_triggers = matches!(
            &status,
            InactiveAgentStatus::Interrupted
                | InactiveAgentStatus::Completed(_)
                | InactiveAgentStatus::Errored(_)
        );
        let _delivery = self.lock_mail_delivery()?;
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
        match &status {
            InactiveAgentStatus::AwaitingDescendants(deferred_turn_id) => {
                if agent.active_durable_turn_id != Some(*deferred_turn_id) {
                    return Err(AgentControlError::StaleExecution(lease.path.clone()));
                }
                if agent.pending_deferred_release.take() == Some(*deferred_turn_id) {
                    promotes_pending_triggers = true;
                }
                agent.pending_deferred_release = None;
                agent.active_durable_turn_id = None;
                agent.awaiting_deferred_turn_id = Some(*deferred_turn_id);
            }
            _ => {
                agent.active_durable_turn_id = None;
                agent.awaiting_deferred_turn_id = None;
                agent.pending_deferred_release = None;
            }
        }
        if promotes_pending_triggers {
            for notice in &mut agent.mailbox {
                if notice.trigger_turn {
                    notice.schedule_ready = true;
                }
            }
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn project_released_deferred_completion(
        &self,
        deferred_path: &AgentPath,
        deferred_session_id: SessionId,
        deferred_turn_id: TurnId,
        parent_path: &AgentPath,
        parent_session_id: SessionId,
        status: InactiveAgentStatus,
        activity: Option<String>,
        history_item_id: HistoryItemId,
        released_owner_deferred_turn_id: Option<TurnId>,
        durable_read: impl FnOnce() -> Result<Option<OwnerResumeRequestId>, String>,
    ) -> Result<Vec<AgentExecutionLease>, AgentControlError> {
        if matches!(
            status,
            InactiveAgentStatus::PendingInit | InactiveAgentStatus::AwaitingDescendants(_)
        ) {
            return Err(AgentControlError::AgentNotAwaitingDescendants(
                deferred_path.clone(),
            ));
        }
        let released_status = AgentStatus::from(status);
        let _delivery = self.lock_mail_delivery()?;
        let owner_resume_request_id =
            durable_read().map_err(AgentControlError::DurableOwnerResumeRead)?;
        let mut state = self.lock()?;
        if state.root_scope_control.is_cancelled() {
            return Err(AgentControlError::TreeCancelled);
        }
        let deferred = state
            .agents
            .get(deferred_path)
            .ok_or_else(|| AgentControlError::AgentNotFound(deferred_path.clone()))?;
        if deferred.session_id != deferred_session_id
            || deferred.parent.as_ref() != Some(parent_path)
        {
            return Err(AgentControlError::AgentNotAwaitingDescendants(
                deferred_path.clone(),
            ));
        }
        if deferred.execution_marker.is_some()
            || !matches!(deferred.status, AgentStatus::AwaitingDescendants)
            || deferred.awaiting_deferred_turn_id != Some(deferred_turn_id)
        {
            // Durable effects are replayable. Once this exact generation was applied, or after a
            // later local generation took ownership, repeating D1 must not publish parent mail,
            // mutate readiness, or finalize D2.
            return Ok(Vec::new());
        }
        // The durable deferred terminal is authoritative for the exact child. Parent projection is
        // only a best-effort process-local wake: a delayed handoff may race a parent restart,
        // shutdown, or a newer OwnerResume identity. Those races must never strand the exact child
        // in AwaitingDescendants or overwrite newer parent ownership.
        let mut projection_warning = None;
        let parent_projection_valid = match state.agents.get(parent_path) {
            None => {
                projection_warning = Some(format!("parent wake skipped: {parent_path} is absent"));
                false
            }
            Some(parent) if parent.session_id != parent_session_id => {
                projection_warning = Some(format!(
                    "parent wake skipped: {parent_path} session identity changed"
                ));
                false
            }
            Some(parent) if matches!(parent.status, AgentStatus::Shutdown) => {
                projection_warning =
                    Some(format!("parent wake skipped: {parent_path} is shutdown"));
                false
            }
            Some(_) => true,
        };
        if parent_projection_valid {
            let (promotes_parent, _owner_resume_schedulable) = {
                let parent = state
                    .agents
                    .get_mut(parent_path)
                    .expect("released deferred parent was validated under the same lock");
                (
                    project_deferred_owner_release(parent, released_owner_deferred_turn_id),
                    reconcile_current_owner_resume_request(parent, owner_resume_request_id),
                )
            };
            let parent = state
                .agents
                .get(parent_path)
                .expect("released deferred parent was validated under the same lock");
            let notice_exists = parent
                .mailbox
                .iter()
                .any(|notice| notice.history_item_id == history_item_id);
            let mailbox_has_capacity = parent.mailbox.len() < MAX_AGENT_MAILBOX_NOTICES;

            if !notice_exists && mailbox_has_capacity {
                let parent = state
                    .agents
                    .get_mut(parent_path)
                    .expect("released deferred parent was validated under the same lock");
                parent.mailbox_generation = parent.mailbox_generation.wrapping_add(1);
                let generation = parent.mailbox_generation;
                parent.mailbox.push_back(AgentMailboxNotice {
                    history_item_id,
                    trigger_turn: false,
                    schedule_ready: false,
                    generation,
                });
                parent.mailbox_activity_tx.send_replace(generation);
            }
            if promotes_parent {
                let parent = state
                    .agents
                    .get_mut(parent_path)
                    .expect("released deferred parent was validated under the same lock");
                for notice in &mut parent.mailbox {
                    if notice.trigger_turn {
                        notice.schedule_ready = true;
                    }
                }
            }
        }
        let deferred = state
            .agents
            .get_mut(deferred_path)
            .expect("released deferred agent was validated under the same lock");
        for notice in &mut deferred.mailbox {
            if notice.trigger_turn {
                notice.schedule_ready = true;
            }
        }
        deferred.status = released_status;
        deferred.last_activity = match (activity, projection_warning) {
            (Some(activity), Some(warning)) => Some(format!("{activity}; {warning}")),
            (None, Some(warning)) => Some(warning),
            (activity, None) => activity,
        };
        deferred.active_durable_turn_id = None;
        deferred.awaiting_deferred_turn_id = None;
        deferred.pending_deferred_release = None;
        let scheduled = self.reserve_pending_triggered_executions_locked(&mut state);
        drop(state);
        self.notify_activity();
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
            || agent.pending_owner_resume_request_id.is_some()
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
                    .all(|agent| !agent_has_live_work(agent))))
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

    pub fn mailbox_has_ready_trigger_turn(
        &self,
        recipient: &AgentPath,
    ) -> Result<bool, AgentControlError> {
        let state = self.lock()?;
        let agent = state
            .agents
            .get(recipient)
            .ok_or_else(|| AgentControlError::AgentNotFound(recipient.clone()))?;
        Ok(agent
            .mailbox
            .iter()
            .any(|message| message.trigger_turn && message.schedule_ready))
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

    /// Captures the exact target execution observed by one exact caller execution.
    ///
    /// The returned capability is safe to retain across a durable commit: a later execution at
    /// the same path has a different marker and run control.
    pub(crate) fn capture_interrupt_target(
        &self,
        caller: &AgentExecutionScope,
        path: &AgentPath,
    ) -> Result<AgentInterruptTarget, AgentControlError> {
        let state = self.lock()?;
        validate_execution_scope_locked(self, &state, caller, caller.path())?;
        let agent = state
            .agents
            .get(path)
            .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
        Ok(AgentInterruptTarget {
            path: path.clone(),
            session_id: agent.session_id,
            status: agent.status.clone(),
            marker: agent.execution_marker.clone(),
            run_control: agent.run_control.clone(),
        })
    }

    /// Commits the caller-owned interruption evidence and then interrupts only the captured target
    /// execution.
    ///
    /// The registry lock keeps caller and target identities stable while the target run-control
    /// classification mutex linearizes `Open -> durable callback -> AgentInterrupted`. The local
    /// boundary bypasses root routing only for this already-validated exact child execution.
    pub(crate) fn commit_and_interrupt_captured(
        &self,
        caller: &AgentExecutionScope,
        target: &AgentInterruptTarget,
        durable_commit: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), AgentControlError> {
        let interrupted = {
            let mut state = self.lock()?;
            validate_execution_scope_locked(self, &state, caller, caller.path())?;
            let current = state
                .agents
                .get(target.path())
                .ok_or_else(|| AgentControlError::AgentNotFound(target.path().clone()))?;
            let same_target = current.session_id == target.session_id
                && match (&current.execution_marker, &target.marker) {
                    (Some(current), Some(captured)) => Arc::ptr_eq(current, captured),
                    (None, None) => true,
                    (Some(_), None) | (None, Some(_)) => false,
                };
            if !same_target {
                return Err(AgentControlError::StaleExecution(target.path().clone()));
            }
            if target.marker.is_some() {
                let cause = crate::runtime::RunCancellationCause::Interruption(
                    TurnInterruptionCause::AgentInterrupted,
                );
                let outcome = target.run_control.commit_cancel_local(cause, || {
                    durable_commit().map_err(AgentControlError::DurableInterruptCommit)
                })?;
                if !matches!(outcome, RunCancelOutcome::Applied) {
                    return Err(AgentControlError::StaleExecution(target.path().clone()));
                }
                let current = state
                    .agents
                    .get_mut(target.path())
                    .expect("captured interrupt target was validated under the same lock");
                clear_cancelled_active_generation_state(current);
                true
            } else {
                durable_commit().map_err(AgentControlError::DurableInterruptCommit)?;
                false
            }
        };
        if interrupted {
            self.notify_activity();
        }
        Ok(())
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
        if run_control.interrupt(TurnInterruptionCause::AgentInterrupted) {
            let mut state = self.lock()?;
            if let Some(agent) = state
                .agents
                .get_mut(path)
                .filter(|agent| agent.run_control.same_owner(&run_control))
            {
                clear_cancelled_active_generation_state(agent);
            }
        }
        self.notify_activity();
        Ok(())
    }

    /// Stops the exact agent whose durable session was terminalized outside the current worker.
    /// Unlike `cancel_agent`, that agent cannot restart from an already queued trigger turn.
    /// Descendants have independent lifecycle owners and are not implicitly stopped.
    pub fn cancel_for_durable_terminal(&self, path: &AgentPath) -> Result<(), AgentControlError> {
        let (
            terminal_session_id,
            terminal_epoch,
            purge_through_generation,
            pending_owner_resume_at_terminal,
        ) = {
            let mut state = self.lock()?;
            let agent = state
                .agents
                .get_mut(path)
                .ok_or_else(|| AgentControlError::AgentNotFound(path.clone()))?;
            // This boundary already owns the agent-tree state lock. Root turns install a terminal
            // router that re-enters the same lock, so route-free local classification is required
            // here; otherwise exact durable terminalization of `/root` deadlocks.
            let _ = agent
                .run_control
                .request_cancel_local(crate::runtime::RunCancellationCause::Superseded);
            clear_cancelled_active_generation_state(agent);
            if agent.trigger_purge_pending == 0 {
                agent.trigger_admission_epoch = agent.trigger_admission_epoch.wrapping_add(1);
            }
            agent.trigger_purge_pending = agent.trigger_purge_pending.saturating_add(1);
            (
                agent.session_id,
                agent.trigger_admission_epoch,
                agent.mailbox_generation,
                agent.pending_owner_resume_request_id,
            )
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
        // Purge only notices that existed when this terminal owner closed the old turn.
        // A session-scoped trigger committed after the durable terminal is newer work and wins;
        // removing it here would make canonical history and the live scheduler disagree.
        agent.mailbox.retain(|message| {
            !message.trigger_turn || message.generation > purge_through_generation
        });
        if agent.pending_owner_resume_request_id == pending_owner_resume_at_terminal {
            agent.pending_owner_resume_request_id = None;
        }
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

    fn route_terminal_outcome(
        &self,
        source: &RunControl,
        kind: RunTerminalRouteKind,
        cause: crate::runtime::RunCancellationCause,
    ) -> Option<RunCancelOutcome> {
        let state = self.lock().ok()?;
        let root = state
            .agents
            .iter()
            .find_map(|(path, agent)| path.is_root().then_some(agent))?;
        let source_is_scope = state.root_scope_control.same_owner(source);
        let source_is_root_turn = root.run_control.same_owner(source);
        if !source_is_scope && !source_is_root_turn {
            return None;
        }
        if source_is_scope && kind != RunTerminalRouteKind::Request {
            return None;
        }
        let root_control = root.run_control.clone();
        drop(state);
        Some(match kind {
            RunTerminalRouteKind::Request => root_control.request_cancel_local(cause),
            RunTerminalRouteKind::ResolveSuccessCommitAuthoritatively => {
                if root_control.resolve_success_commit_authoritatively_local(cause) {
                    RunCancelOutcome::Applied
                } else {
                    RunCancelOutcome::Rejected
                }
            }
            RunTerminalRouteKind::AbandonSuccessCommit => {
                if root_control.abandon_success_commit_local(cause).is_some() {
                    RunCancelOutcome::Applied
                } else {
                    RunCancelOutcome::Rejected
                }
            }
            // `RunControl::release_success_commit` has already published the pending cause
            // locally before routing this notification. No retained-tree state participates.
            RunTerminalRouteKind::ReleaseSuccessCommit => RunCancelOutcome::Applied,
        })
    }

    fn cancel_tree_with_root_cause(&self, root_cause: TurnInterruptionCause) -> bool {
        self.classify_tree(
            crate::runtime::RunCancellationCause::Interruption(root_cause),
            crate::runtime::RunCancellationCause::Interruption(TurnInterruptionCause::TreeStopped),
        )
    }

    fn classify_tree(
        &self,
        root_cause: crate::runtime::RunCancellationCause,
        descendant_cause: crate::runtime::RunCancellationCause,
    ) -> bool {
        self.classify_tree_result(root_cause, descendant_cause)
            .changed()
    }

    fn classify_tree_result(
        &self,
        root_cause: crate::runtime::RunCancellationCause,
        descendant_cause: crate::runtime::RunCancellationCause,
    ) -> TreeClassificationResult {
        let Ok(_spawn_tree_fence) = self.lock_spawn_tree_fence() else {
            return TreeClassificationResult::rejected();
        };
        let Ok(mut state) = self.lock() else {
            return TreeClassificationResult::rejected();
        };
        let Some(root) = state
            .agents
            .iter()
            .find_map(|(path, agent)| path.is_root().then_some(agent))
        else {
            return TreeClassificationResult::rejected();
        };
        let root_success_is_durable = root.run_control.success_is_sealed();
        if root_success_is_durable {
            if state
                .root_scope_control
                .cause()
                .is_some_and(|existing| existing != root_cause)
            {
                return TreeClassificationResult::rejected();
            }
            let scope_outcome = state
                .root_scope_control
                .request_cancel_local(root_cause.clone());
            let scope_applied = scope_outcome == RunCancelOutcome::Applied;
            let scope_owns_requested_cause =
                scope_applied || state.root_scope_control.cause().as_ref() == Some(&root_cause);
            if !scope_owns_requested_cause {
                return TreeClassificationResult::rejected();
            }
            for (path, agent) in &mut state.agents {
                if !path.is_root() {
                    agent.run_control.cancel(descendant_cause.clone());
                }
                if agent.run_control.cause().is_some() {
                    clear_cancelled_active_generation_state(agent);
                }
            }
            drop(state);
            self.notify_activity();
            return TreeClassificationResult {
                root_outcome: RunCancelOutcome::Rejected,
                tree_applied: scope_applied,
            };
        }
        if state
            .root_scope_control
            .cause()
            .is_some_and(|existing| existing != root_cause)
        {
            return TreeClassificationResult::rejected();
        }
        let root_outcome = root.run_control.request_cancel_local(root_cause.clone());
        let root_owns_requested_cause =
            matches!(root_outcome, crate::runtime::RunCancelOutcome::Applied)
                || root.run_control.cause().as_ref() == Some(&root_cause);
        let deferred_tree_action =
            matches!(root_outcome, crate::runtime::RunCancelOutcome::Deferred(_));
        if !root_owns_requested_cause && !deferred_tree_action {
            return TreeClassificationResult {
                root_outcome,
                tree_applied: false,
            };
        }
        let scope_outcome = state
            .root_scope_control
            .request_cancel_local(root_cause.clone());
        let scope_applied = scope_outcome == RunCancelOutcome::Applied;
        let scope_owns_requested_cause =
            scope_applied || state.root_scope_control.cause().as_ref() == Some(&root_cause);
        if !scope_owns_requested_cause {
            return TreeClassificationResult {
                root_outcome,
                tree_applied: false,
            };
        }
        for (path, agent) in &mut state.agents {
            if !path.is_root() {
                agent.run_control.cancel(descendant_cause.clone());
            }
            if agent.run_control.cause().is_some() {
                clear_cancelled_active_generation_state(agent);
            }
        }
        drop(state);
        self.notify_activity();
        TreeClassificationResult {
            root_outcome,
            tree_applied: scope_applied,
        }
    }

    pub fn tree_is_cancelled(&self) -> bool {
        self.lock()
            .is_ok_and(|state| state.root_scope_control.is_cancelled())
    }

    /// Returns exact process-local executions that have received a terminal cancellation but have
    /// not yet released their worker lease.
    ///
    /// Cancellation classification and hard task abortion intentionally remain separate phases:
    /// callers first give these workers the Codex-compatible cooperative grace period, then abort
    /// only generations still present in this list.
    pub(crate) fn cancelled_execution_paths(&self) -> Result<Vec<AgentPath>, AgentControlError> {
        let state = self.lock()?;
        Ok(state
            .agents
            .iter()
            .filter_map(|(path, agent)| {
                (agent.execution_marker.is_some() && agent.run_control.is_cancelled())
                    .then(|| path.clone())
            })
            .collect())
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
            agent.active_durable_turn_id = None;
            agent.awaiting_deferred_turn_id = None;
            agent.pending_deferred_release = None;
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

    fn lock_spawn_tree_fence(&self) -> Result<MutexGuard<'_, ()>, AgentControlError> {
        self.inner
            .spawn_tree_fence
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
                    && pending_execution_wake_cause(agent).is_some())
                .then_some((agent.spawn_order, path.clone()))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(spawn_order, _)| *spawn_order);
        let mut leases = Vec::new();
        for (_, path) in candidates {
            if active_agent_count(state) >= descendant_capacity(state) {
                break;
            }
            let wake_cause = state
                .agents
                .get(&path)
                .and_then(pending_execution_wake_cause)
                .expect("a scheduled agent must retain one durable wake owner");
            let marker = Arc::new(());
            let run_control = RunControl::new();
            let agent = state
                .agents
                .get_mut(&path)
                .expect("scheduled agent was selected from this registry");
            agent.execution_marker = Some(marker.clone());
            agent.run_control = run_control.clone();
            agent.status = AgentStatus::PendingInit;
            agent.active_durable_turn_id = None;
            agent.awaiting_deferred_turn_id = None;
            agent.pending_deferred_release = None;
            leases.push(AgentExecutionLease {
                control: self.clone(),
                path,
                marker,
                run_control,
                wake_cause: Some(wake_cause),
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

    /// Returns the canonical session-scoped history item that admitted this execution.
    ///
    /// Root turns and directly acquired/manual leases have no trigger identity. Initial child
    /// turns bind their durable task identity after the atomic spawn commit, while mailbox-driven
    /// turns receive it when the scheduler reserves the lease.
    pub(crate) fn wake_cause(&self) -> Option<AgentExecutionWakeCause> {
        self.wake_cause
    }

    pub fn trigger_history_item_id(&self) -> Option<HistoryItemId> {
        match self.wake_cause {
            Some(AgentExecutionWakeCause::ExplicitTask(history_item_id)) => Some(history_item_id),
            Some(AgentExecutionWakeCause::OwnerResume(_)) | None => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn owner_resume_request_id(&self) -> Option<OwnerResumeRequestId> {
        match self.wake_cause {
            Some(AgentExecutionWakeCause::OwnerResume(request_id)) => Some(request_id),
            Some(AgentExecutionWakeCause::ExplicitTask(_)) | None => None,
        }
    }

    /// Binds the canonical trigger to an initial child lease without allowing replacement.
    ///
    /// The lease is consumed so the unbound capability cannot keep circulating after a durable
    /// task has been associated with it. On duplicate binding the original lease is returned
    /// unchanged, preserving its execution ownership for explicit caller recovery.
    pub(crate) fn try_bind_trigger_history_item_id(
        mut self,
        history_item_id: HistoryItemId,
    ) -> Result<Self, Self> {
        if self.wake_cause.is_some() {
            return Err(self);
        }
        self.wake_cause = Some(AgentExecutionWakeCause::ExplicitTask(history_item_id));
        Ok(self)
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

fn validate_execution_scope_locked(
    control: &AgentControl,
    state: &AgentTreeState,
    scope: &AgentExecutionScope,
    expected_path: &AgentPath,
) -> Result<(), AgentControlError> {
    if !Arc::ptr_eq(&control.inner, &scope.control.inner) || &scope.path != expected_path {
        return Err(AgentControlError::StaleExecution(expected_path.clone()));
    }
    let marker = scope
        .marker
        .upgrade()
        .ok_or_else(|| AgentControlError::StaleExecution(expected_path.clone()))?;
    let agent = state
        .agents
        .get(expected_path)
        .ok_or_else(|| AgentControlError::AgentNotFound(expected_path.clone()))?;
    if !agent
        .execution_marker
        .as_ref()
        .is_some_and(|active| Arc::ptr_eq(active, &marker))
        || agent.run_control.is_cancelled()
        || agent.run_control.success_is_sealed()
    {
        return Err(AgentControlError::StaleExecution(expected_path.clone()));
    }
    Ok(())
}

fn validate_child_registration_locked(
    state: &AgentTreeState,
    parent: &AgentPath,
    child_path: &AgentPath,
    session_id: SessionId,
) -> Result<(), AgentControlError> {
    if state.root_scope_control.is_cancelled() {
        return Err(AgentControlError::TreeCancelled);
    }
    if !state.agents.contains_key(parent) {
        return Err(AgentControlError::AgentNotFound(parent.clone()));
    }
    if state.agents.contains_key(child_path) {
        return Err(AgentControlError::AgentAlreadyExists(child_path.clone()));
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
    if active_agent_count(state) >= descendant_capacity(state) {
        return Err(AgentControlError::AgentLimitReached {
            max_concurrent_agents: state.max_concurrent_agents,
        });
    }
    Ok(())
}

fn insert_child_locked(
    state: &mut AgentTreeState,
    parent: &AgentPath,
    child_path: AgentPath,
    session_id: SessionId,
    initial_activity: Option<String>,
    durable_spawn_order: Option<u64>,
) -> Result<(AgentSnapshot, Arc<()>, RunControl), AgentControlError> {
    let spawn_order = match durable_spawn_order {
        Some(order)
            if order == 0
                || state
                    .agents
                    .values()
                    .any(|agent| agent.spawn_order == order) =>
        {
            return Err(AgentControlError::SpawnOrderAlreadyUsed(order));
        }
        Some(order) => {
            state.next_spawn_order = state.next_spawn_order.max(
                order
                    .checked_add(1)
                    .ok_or(AgentControlError::SpawnOrderExhausted)?,
            );
            order
        }
        None => allocate_spawn_order(state)?,
    };
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
            pending_owner_resume_request_id: None,
            active_durable_turn_id: None,
            awaiting_deferred_turn_id: None,
            pending_deferred_release: None,
            mailbox_generation: 0,
            trigger_admission_epoch: 0,
            trigger_purge_pending: 0,
            mailbox_activity_tx,
        },
    );
    let snapshot = snapshot_agent(state, &child_path)
        .expect("a child inserted into the registry must be available for its snapshot");
    Ok((snapshot, marker, run_control))
}

fn active_agent_count(state: &AgentTreeState) -> usize {
    state.pending_capacity_reservations
        + state
            .agents
            .iter()
            .filter(|(path, agent)| !path.is_root() && agent.execution_marker.is_some())
            .count()
}

fn descendant_capacity(state: &AgentTreeState) -> usize {
    state.max_concurrent_agents.saturating_sub(1)
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
    agent.execution_marker.is_some()
        || matches!(agent.status, AgentStatus::AwaitingDescendants)
        || pending_execution_wake_cause(agent).is_some()
}

fn clear_cancelled_active_generation_state(agent: &mut AgentEntry) {
    agent.active_durable_turn_id = None;
    agent.pending_deferred_release = None;
}

/// Applies one durable deferred-owner release only to its exact process-local generation.
///
/// A release can arrive after durable terminal commit but before `complete_execution` publishes
/// AwaitingDescendants. In that narrow window it is retained in a one-shot deferred-release latch.
/// A later generation never consumes an older turn's latch.
fn project_deferred_owner_release(
    agent: &mut AgentEntry,
    released_turn_id: Option<TurnId>,
) -> bool {
    let Some(released_turn_id) = released_turn_id else {
        return false;
    };
    if matches!(agent.status, AgentStatus::Shutdown) || agent.run_control.cause().is_some() {
        return false;
    }
    if agent.execution_marker.is_some() {
        if agent.active_durable_turn_id == Some(released_turn_id) {
            agent.pending_deferred_release = Some(released_turn_id);
        }
        return false;
    }
    if !matches!(agent.status, AgentStatus::AwaitingDescendants)
        || agent.awaiting_deferred_turn_id != Some(released_turn_id)
    {
        return false;
    }
    true
}

/// Projects the current durable OwnerResume scheduler identity without changing explicit-mail
/// readiness. A conflicting in-memory identity is left untouched; the caller's storage snapshot
/// may have raced a newer durable transition.
fn project_owner_resume_request(
    agent: &mut AgentEntry,
    request_id: Option<OwnerResumeRequestId>,
) -> bool {
    let Some(request_id) = request_id else {
        return false;
    };
    match agent.pending_owner_resume_request_id {
        Some(existing) => existing == request_id,
        None => {
            agent.pending_owner_resume_request_id = Some(request_id);
            true
        }
    }
}

/// Reconciles process-local scheduler state to one current durable read. Unlike ordinary mail
/// projection, `None` authoritatively clears a previously captured request and `Some(R2)`
/// replaces R1.
fn reconcile_current_owner_resume_request(
    agent: &mut AgentEntry,
    request_id: Option<OwnerResumeRequestId>,
) -> bool {
    agent.pending_owner_resume_request_id = request_id;
    request_id.is_some()
}

fn pending_execution_wake_cause(agent: &AgentEntry) -> Option<AgentExecutionWakeCause> {
    let ready_explicit = agent
        .mailbox
        .iter()
        .enumerate()
        .filter(|(_, notice)| notice.trigger_turn && notice.schedule_ready)
        .max_by_key(|(index, notice)| (notice.generation, *index))
        .map(|(_, notice)| AgentExecutionWakeCause::ExplicitTask(notice.history_item_id));
    if ready_explicit.is_some() {
        return ready_explicit;
    }
    if agent.mailbox.iter().any(|notice| notice.trigger_turn) {
        return None;
    }
    agent
        .pending_owner_resume_request_id
        .map(AgentExecutionWakeCause::OwnerResume)
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
        let outcome = control.commit_and_enqueue_mail(author, recipient, trigger_turn, || {
            Ok(AgentMailCommit {
                history_item_id,
                schedule_turn: trigger_turn,
                owner_resume_request_id: None,
            })
        })?;
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
    fn exact_root_stop_during_success_commit_preserves_child_and_future_continuation() {
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
        assert_eq!(root_scope.cause(), None);
        assert_eq!(child_turn.cause(), None);
        assert!(!control.tree_is_cancelled());
        assert!(success.seal());
        assert!(root_turn.success_is_sealed());
        control
            .complete_execution(root_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete durable success");

        let continuation =
            admitted_continuation(control.try_acquire_root_continuation(root_scope.clone()));
        assert_eq!(continuation.run_control().cause(), None);
        assert_eq!(child_turn.cause(), None);
        assert!(!control.tree_is_cancelled());
        control
            .complete_execution(continuation, InactiveAgentStatus::Completed(None), None)
            .expect("complete continuation");
        control
            .complete_execution(child_execution, InactiveAgentStatus::Completed(None), None)
            .expect("settle child");
    }

    #[test]
    fn exact_root_stop_cancels_fresh_continuation_without_cancelling_tree_scope() {
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
        assert_eq!(root_scope.cause(), None);
        assert!(!control.tree_is_cancelled());
        control
            .complete_execution(continuation, InactiveAgentStatus::Interrupted, None)
            .expect("settle stopped continuation");
    }

    #[test]
    fn continuation_admission_depends_on_the_root_owner_not_descendant_liveness() {
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
            AgentRootContinuationOutcome::Invalid
        ));
        assert!(root_turn.seal_success());
        control
            .complete_execution(root_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete root");
        let continuation = admitted_continuation(control.try_acquire_root_continuation(root_scope));
        assert!(!continuation.run_control().same_owner(&root_turn));
        assert!(
            control
                .list_agents(Some(&child.path))
                .expect("child snapshot")
                .into_iter()
                .any(|snapshot| snapshot.path == child.path && snapshot.is_active),
            "the retained child remains independently owned while root continuation starts"
        );
        drop(continuation);
        drop(child_execution);
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
    fn root_and_child_failures_each_remain_exact_task_local() {
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
        assert_eq!(root_turn.cause(), Some(failure));
        assert_eq!(root_scope.cause(), None);
        assert_eq!(sibling_turn.cause(), None);
        assert!(!control.tree_is_cancelled());
        assert!(sibling_turn.begin_tool_effect_admission().is_some());
    }

    #[test]
    fn root_failure_during_tool_settlement_stays_local_before_and_after_release() {
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
        assert_eq!(root_scope.cause(), None);
        assert_eq!(child_turn.cause(), None);
        assert!(!control.tree_is_cancelled());
        assert!(child_turn.begin_tool_effect_admission().is_some());
        assert_eq!(root_turn.cause(), None);
        settlement.release();
        assert_eq!(root_turn.cause(), Some(failure));
        assert_eq!(root_scope.cause(), None);
        assert_eq!(child_turn.cause(), None);
        assert!(!control.tree_is_cancelled());
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
        let reviewer = worker.join("reviewer").expect("reviewer path");
        assert_eq!(worker.as_str(), "/root/worker_1");
        assert_eq!(reviewer.as_str(), "/root/worker_1/reviewer");
        assert_eq!(worker.name(), "worker_1");
        assert_eq!(worker.parent(), Some(AgentPath::root()));
        assert_eq!(reviewer.parent(), Some(worker.clone()));
        assert_eq!(worker.resolve("reviewer").expect("relative path"), reviewer);
        assert_eq!(
            worker.resolve("/root/other").expect("absolute path"),
            AgentPath::try_from("/root/other").expect("canonical path")
        );

        assert!(AgentPath::root().join("BadName").is_err());
        assert!(AgentPath::root().join("two/parts").is_err());
        assert!(AgentPath::try_from("/other/worker").is_err());
        assert!(AgentPath::try_from("/root/worker/").is_err());
        assert!(AgentPath::root().resolve("../sibling").is_err());
        assert_eq!(
            worker
                .resolve("review/retry")
                .expect("relative descendants"),
            AgentPath::try_from("/root/worker_1/review/retry").expect("canonical path")
        );
    }

    #[test]
    fn every_agent_can_register_and_restore_a_child() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 4).expect("agent tree");
        let root = AgentPath::root();
        let (child, _child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("direct child");
        let (grandchild, _grandchild_execution) = control
            .register_child(&child.path, "nested", SessionId::new(), None)
            .expect("nested child");
        let restored = control
            .restore_inactive_child(
                &grandchild.path,
                "restored_nested",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("restored nested child");

        assert_eq!(grandchild.path.as_str(), "/root/worker/nested");
        assert_eq!(
            restored.path.as_str(),
            "/root/worker/nested/restored_nested"
        );
    }

    #[test]
    fn public_capacity_includes_root_and_internal_pool_counts_only_descendants() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 4).expect("agent tree");
        let root = AgentPath::root();
        let (_, first) = control
            .register_child(&root, "first", SessionId::new(), None)
            .expect("first descendant");
        let (_, second) = control
            .register_child(&root, "second", SessionId::new(), None)
            .expect("second descendant");
        let (_, third) = control
            .register_child(&root, "third", SessionId::new(), None)
            .expect("third descendant");

        let fourth_result = control.register_child(&root, "fourth", SessionId::new(), None);
        assert!(matches!(
            fourth_result,
            Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: 4
            })
        ));
        assert_eq!(control.snapshot().expect("snapshot").active_agent_count, 3);

        drop(root_execution);
        assert_eq!(control.snapshot().expect("snapshot").active_agent_count, 3);
        drop(first);
        drop(second);
        drop(third);
        assert_eq!(control.snapshot().expect("snapshot").active_agent_count, 0);

        let (root_only, _root_execution) =
            AgentControl::new(SessionId::new(), 1).expect("root-only tree");
        assert!(matches!(
            root_only.register_child(&AgentPath::root(), "blocked", SessionId::new(), None,),
            Err(AgentControlError::AgentLimitReached {
                max_concurrent_agents: 1
            })
        ));
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
    fn new_root_turn_can_reenter_while_a_descendant_keeps_running() {
        let root_scope = RunControl::new();
        let (control, root_execution) =
            AgentControl::with_root_control(SessionId::new(), 2, root_scope).expect("agent tree");
        let (_, child_execution) = control
            .register_child(
                &AgentPath::root(),
                "worker",
                SessionId::new(),
                Some("detached work".to_string()),
            )
            .expect("child execution");
        let child_control = child_execution.run_control();
        control
            .complete_execution(
                root_execution,
                InactiveAgentStatus::Completed(Some("root result".to_string())),
                None,
            )
            .expect("complete first root turn");

        let replacement_scope = RunControl::new();
        let replacement_root = control
            .try_acquire_root_execution(replacement_scope.clone())
            .expect("new root turn while child remains active");
        assert_eq!(
            control.snapshot().expect("active tree").active_agent_count,
            1
        );

        assert!(replacement_scope.interrupt(TurnInterruptionCause::UserStop));
        assert_eq!(
            replacement_root.run_control().cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert_eq!(
            child_control.cause(),
            None,
            "ordinary root Stop must not cancel an independently running child"
        );
        assert!(!control.tree_is_cancelled());
        control
            .complete_execution(replacement_root, InactiveAgentStatus::Interrupted, None)
            .expect("settle exact stopped root");
        control
            .complete_execution(child_execution, InactiveAgentStatus::Completed(None), None)
            .expect("settle independent child");
    }

    #[test]
    fn durable_spawn_and_tree_stop_are_serialized_by_exact_execution_scope() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root_scope = root_execution.scope();
        let (commit_entered_tx, commit_entered_rx) = std::sync::mpsc::channel();
        let (release_commit_tx, release_commit_rx) = std::sync::mpsc::channel();
        let spawn_control = control.clone();
        let spawn_thread = std::thread::spawn(move || {
            spawn_control.commit_spawn(
                &root_scope,
                &AgentPath::root(),
                "worker",
                SessionId::new(),
                Some("durably committed".to_string()),
                || {
                    commit_entered_tx.send(()).expect("commit entered signal");
                    release_commit_rx.recv().expect("release durable commit");
                    Ok::<_, String>(((), 1))
                },
            )
        });
        commit_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("spawn reached durable commit");

        let stop_control = control.clone();
        let (stop_done_tx, stop_done_rx) = std::sync::mpsc::channel();
        let stop_thread = std::thread::spawn(move || {
            let stopped = stop_control.interrupt_tree(TurnInterruptionCause::UserStop);
            stop_done_tx.send(stopped).expect("stop result");
        });
        assert!(
            stop_done_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "Stop must wait until the winning spawn is durably committed and registered"
        );

        release_commit_tx.send(()).expect("release durable commit");
        let (_, child, child_execution) = spawn_thread
            .join()
            .expect("spawn thread")
            .expect("spawn commit");
        assert_eq!(child.path.as_str(), "/root/worker");
        assert!(
            stop_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("Stop completion")
        );
        stop_thread.join().expect("stop thread");
        assert_eq!(
            child_execution.run_control().cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );

        let commit_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let commit_called_in_closure = Arc::clone(&commit_called);
        let stopped_spawn = control.commit_spawn(
            &root_execution.scope(),
            &AgentPath::root(),
            "late",
            SessionId::new(),
            None,
            || {
                commit_called_in_closure.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, String>(((), 2))
            },
        );
        assert!(stopped_spawn.is_err());
        assert!(!commit_called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn root_manual_and_initial_child_leases_start_without_a_trigger_identity() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        assert_eq!(root_execution.trigger_history_item_id(), None);

        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("initial child");
        assert_eq!(child_execution.trigger_history_item_id(), None);
        control
            .complete_execution(child_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete initial child turn");

        let manual_execution = control
            .try_acquire_execution(&child.path)
            .expect("direct manual execution");
        assert_eq!(manual_execution.trigger_history_item_id(), None);
    }

    #[test]
    fn initial_child_trigger_identity_can_be_bound_once_without_losing_the_lease() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root");
        let (_, child_execution) = control
            .register_child(&AgentPath::root(), "worker", SessionId::new(), None)
            .expect("initial child");
        let initial_task_id = HistoryItemId::new();
        let child_execution =
            match child_execution.try_bind_trigger_history_item_id(initial_task_id) {
                Ok(lease) => lease,
                Err(_) => panic!("an unbound initial lease must accept its durable task identity"),
            };
        assert_eq!(
            child_execution.trigger_history_item_id(),
            Some(initial_task_id)
        );

        let replacement_id = HistoryItemId::new();
        let child_execution = match child_execution.try_bind_trigger_history_item_id(replacement_id)
        {
            Ok(_) => panic!("a canonical trigger identity must never be replaced"),
            Err(lease) => lease,
        };
        assert_eq!(
            child_execution.trigger_history_item_id(),
            Some(initial_task_id)
        );
        child_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("the rejected rebind must return the still-live execution lease");
    }

    #[test]
    fn pending_trigger_scheduler_binds_the_latest_generation_exactly_once() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root owner");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        control
            .complete_execution(child_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete initial child turn");

        let older_trigger_id = HistoryItemId::new();
        let latest_trigger_id = HistoryItemId::new();
        control
            .restore_pending_mail(&child.path, older_trigger_id, true)
            .expect("older canonical trigger");
        control
            .restore_pending_mail(&child.path, latest_trigger_id, true)
            .expect("latest canonical trigger");
        let (later_informational_id, _) = enqueue_test_notice(&control, &root, &child.path, false)
            .expect("later informational input");

        let mut scheduled = control
            .schedule_pending_triggered_executions()
            .expect("schedule pending child");
        assert_eq!(scheduled.len(), 1);
        let execution = scheduled.pop().expect("single scheduled execution");
        assert_eq!(execution.trigger_history_item_id(), Some(latest_trigger_id));
        assert!(
            control
                .schedule_pending_triggered_executions()
                .expect("active child is not reserved twice")
                .is_empty()
        );

        let drained = control
            .drain_mailbox(&child.path)
            .expect("claim both canonical inputs");
        assert_eq!(
            drained
                .iter()
                .map(|notice| notice.history_item_id)
                .collect::<Vec<_>>(),
            vec![older_trigger_id, latest_trigger_id, later_informational_id]
        );
        assert!(
            control
                .complete_execution(execution, InactiveAgentStatus::Completed(None), None)
                .expect("complete scheduled turn")
                .is_empty()
        );
    }

    #[test]
    fn ready_existing_agent_trigger_rejects_at_capacity_before_durable_append() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root owner");
        let root = AgentPath::root();
        let target = control
            .restore_inactive_child(
                &root,
                "target",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("retained target");
        let (_sibling, _sibling_execution) = control
            .register_child(&root, "sibling", SessionId::new(), None)
            .expect("capacity-filling sibling");

        let readiness_checked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closure_checked = Arc::clone(&readiness_checked);
        let error = match control.commit_and_enqueue_mail_with_capacity(
            &root_execution.scope(),
            &root,
            &target.path,
            true,
            move |capacity_granted| {
                assert!(!capacity_granted);
                closure_checked.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(AgentControlError::AgentLimitReached {
                    max_concurrent_agents: 2,
                })
            },
        ) {
            Err(error) => error,
            Ok(_) => panic!("ready follow-up must reject before durable append"),
        };
        assert_eq!(
            error,
            AgentControlError::AgentLimitReached {
                max_concurrent_agents: 2
            }
        );
        assert!(
            readiness_checked.load(std::sync::atomic::Ordering::SeqCst),
            "storage must receive the denied capacity grant before deciding readiness"
        );
        assert!(
            control
                .mailbox_history_item_ids(&target.path)
                .expect("target mailbox")
                .is_empty()
        );
    }

    #[test]
    fn dormant_existing_agent_trigger_queues_at_capacity_and_runs_after_release() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root owner");
        let root = AgentPath::root();
        let target_deferred_turn_id = TurnId::new();
        let target = control
            .restore_inactive_child(
                &root,
                "target",
                SessionId::new(),
                InactiveAgentStatus::AwaitingDescendants(target_deferred_turn_id),
                None,
            )
            .expect("retained target");
        let (_sibling, sibling_execution) = control
            .register_child(&root, "sibling", SessionId::new(), None)
            .expect("capacity-filling sibling");
        let trigger = HistoryItemId::new();
        let delivery = control
            .commit_and_enqueue_mail_with_capacity(
                &root_execution.scope(),
                &root,
                &target.path,
                true,
                |capacity_granted| {
                    assert!(!capacity_granted);
                    Ok(AgentMailCommit {
                        history_item_id: trigger,
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                },
            )
            .expect("dormant completed-early follow-up queues at capacity");
        let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = delivery;
        assert!(scheduled.is_empty());
        assert!(
            !control
                .mailbox_has_ready_trigger_turn(&target.path)
                .expect("dormant target trigger")
        );
        assert!(
            control
                .complete_execution(
                    sibling_execution,
                    InactiveAgentStatus::Completed(None),
                    None,
                )
                .expect("free sibling slot")
                .is_empty(),
            "capacity alone must not wake a dormant completed-early trigger"
        );
        let release = control
            .commit_and_enqueue_completion_handoff(
                &root,
                &target.path,
                Some(target_deferred_turn_id),
                || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: false,
                        owner_resume_request_id: Some(OwnerResumeRequestId::from(
                            HistoryItemId::new(),
                        )),
                    })
                },
            )
            .expect("descendant release projection");
        let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = release;
        assert_eq!(scheduled.len(), 1);
        let target_execution = scheduled.pop().expect("queued target execution");
        assert_eq!(target_execution.path(), &target.path);
        assert_eq!(
            target_execution.trigger_history_item_id(),
            Some(trigger),
            "scheduler must reserve the exact durable trigger after capacity frees"
        );
    }

    #[test]
    fn shutdown_target_rejects_followup_before_durable_commit() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 1).expect("agent tree");
        let root = AgentPath::root();
        let target = control
            .restore_inactive_child(
                &root,
                "stopped",
                SessionId::new(),
                InactiveAgentStatus::Shutdown,
                None,
            )
            .expect("retained stopped target");
        let commit_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closure_called = Arc::clone(&commit_called);

        let error = match control.commit_and_enqueue_mail(&root, &target.path, true, move || {
            closure_called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(AgentMailCommit {
                history_item_id: HistoryItemId::new(),
                schedule_turn: true,
                owner_resume_request_id: None,
            })
        }) {
            Err(error) => error,
            Ok(_) => panic!("shutdown target must reject follow-up"),
        };

        assert_eq!(error, AgentControlError::AgentShutdown(target.path));
        assert!(
            !commit_called.load(std::sync::atomic::Ordering::SeqCst),
            "shutdown rejection must precede canonical history mutation"
        );
    }

    #[test]
    fn awaiting_descendants_queues_explicit_trigger_until_owner_resume_wake() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let target_deferred_turn_id = TurnId::new();
        let target = control
            .restore_inactive_child(
                &root,
                "waiting",
                SessionId::new(),
                InactiveAgentStatus::AwaitingDescendants(target_deferred_turn_id),
                None,
            )
            .expect("retained waiting target");
        let trigger = HistoryItemId::new();
        let delivery = control
            .commit_and_enqueue_mail(&root, &target.path, true, || {
                Ok(AgentMailCommit {
                    history_item_id: trigger,
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("completed-early owner accepts queued follow-up");
        let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = delivery;
        assert!(
            scheduled.is_empty(),
            "storage readiness must withhold immediate execution"
        );
        let waiting = control
            .list_agents(Some(&target.path))
            .expect("waiting snapshot")
            .into_iter()
            .next()
            .expect("waiting target");
        assert_eq!(waiting.status, AgentStatus::AwaitingDescendants);
        assert!(!waiting.is_active);
        assert_eq!(waiting.pending_mail_count, 1);
        assert!(
            !control
                .mailbox_has_ready_trigger_turn(&target.path)
                .expect("dormant queued trigger")
        );
        assert!(
            control
                .schedule_pending_triggered_executions()
                .expect("unrelated scheduler pass")
                .is_empty(),
            "a dormant completed-early trigger must not run before descendant settlement"
        );

        let release = control
            .commit_and_enqueue_completion_handoff(
                &root,
                &target.path,
                Some(target_deferred_turn_id),
                || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                },
            )
            .expect("descendant-result wake");
        assert!(
            control
                .mailbox_has_ready_trigger_turn(&target.path)
                .expect("promoted queued trigger")
        );
        let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = release;
        assert_eq!(scheduled.len(), 1);
        let execution = scheduled.pop().expect("explicit recovery execution");
        assert_eq!(execution.path(), &target.path);
        assert_eq!(execution.trigger_history_item_id(), Some(trigger));
        assert_eq!(
            execution.owner_resume_request_id(),
            None,
            "queued explicit work must take precedence over OwnerResume"
        );
        control
            .mark_execution_admitted(
                &execution.scope(),
                AgentExecutionWakeCause::ExplicitTask(trigger),
                TurnId::new(),
                Some("explicit task admitted after descendant settlement".to_string()),
                || Ok(None),
            )
            .expect("admit queued explicit task");
        let notices = control
            .drain_mailbox(&target.path)
            .expect("claim queued explicit task");
        assert_eq!(notices.len(), 2);
        let explicit_notice = notices
            .iter()
            .find(|notice| notice.history_item_id == trigger)
            .expect("queued explicit notice");
        assert!(explicit_notice.trigger_turn);
        assert!(
            notices
                .iter()
                .any(|notice| !notice.trigger_turn && notice.history_item_id != trigger)
        );
        assert!(
            control
                .complete_execution(execution, InactiveAgentStatus::Completed(None), None)
                .expect("complete explicit task")
                .is_empty(),
            "explicit admission must coalesce the pending OwnerResume"
        );
    }

    #[test]
    fn awaiting_crash_recovery_schedules_explicit_trigger_immediately() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let target_deferred_turn_id = TurnId::new();
        let target = control
            .restore_inactive_child(
                &root,
                "crashed",
                SessionId::new(),
                InactiveAgentStatus::AwaitingDescendants(target_deferred_turn_id),
                None,
            )
            .expect("retained crashed target");
        let owner_resume = OwnerResumeRequestId::from(HistoryItemId::new());
        control
            .restore_pending_owner_resume(&target.path, owner_resume)
            .expect("pending crash OwnerResume");
        let trigger = HistoryItemId::new();

        let delivery = control
            .commit_and_enqueue_mail(&root, &target.path, true, || {
                Ok(AgentMailCommit {
                    history_item_id: trigger,
                    schedule_turn: true,
                    owner_resume_request_id: None,
                })
            })
            .expect("durable crash recovery follow-up");
        let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = delivery;
        assert_eq!(scheduled.len(), 1);
        let execution = scheduled.pop().expect("immediate explicit recovery");
        assert_eq!(execution.path(), &target.path);
        assert_eq!(execution.trigger_history_item_id(), Some(trigger));
        assert_eq!(
            execution.owner_resume_request_id(),
            None,
            "ExplicitTask must win over the pending crash OwnerResume"
        );
        let notice = control
            .drain_mailbox(&target.path)
            .expect("crash recovery mailbox")
            .pop()
            .expect("crash recovery notice");
        assert!(notice.trigger_turn);
        assert_eq!(notice.history_item_id, trigger);
    }

    #[test]
    fn dormant_explicit_trigger_fences_owner_resume_until_storage_promotes_it() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let target_deferred_turn_id = TurnId::new();
        let target = control
            .restore_inactive_child(
                &AgentPath::root(),
                "waiting",
                SessionId::new(),
                InactiveAgentStatus::AwaitingDescendants(target_deferred_turn_id),
                None,
            )
            .expect("retained waiting target");
        control
            .restore_pending_mail(&target.path, HistoryItemId::new(), false)
            .expect("dormant explicit trigger");
        control
            .restore_pending_owner_resume(
                &target.path,
                OwnerResumeRequestId::from(HistoryItemId::new()),
            )
            .expect("raw pending OwnerResume");

        assert!(
            control
                .schedule_pending_triggered_executions()
                .expect("scheduler pass")
                .is_empty(),
            "a dormant explicit trigger must fence rather than fall through to OwnerResume"
        );
        let snapshot = control
            .list_agents(Some(&target.path))
            .expect("target snapshot")
            .into_iter()
            .next()
            .expect("target");
        assert_eq!(snapshot.status, AgentStatus::AwaitingDescendants);
        assert!(!snapshot.is_active);
    }

    #[test]
    fn awaiting_durable_commit_failure_leaves_memory_unchanged() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let target_deferred_turn_id = TurnId::new();
        let target = control
            .restore_inactive_child(
                &root,
                "durable_failure",
                SessionId::new(),
                InactiveAgentStatus::AwaitingDescendants(target_deferred_turn_id),
                None,
            )
            .expect("retained deferred target");
        let before = control
            .list_agents(Some(&target.path))
            .expect("snapshot before rejection")
            .into_iter()
            .next()
            .expect("deferred target");
        let commit_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closure_called = Arc::clone(&commit_called);

        let error = match control.commit_and_enqueue_mail(&root, &target.path, true, move || {
            closure_called.store(true, std::sync::atomic::Ordering::SeqCst);
            Err("injected durable validation failure".to_string())
        }) {
            Err(error) => error,
            Ok(_) => panic!("failed durable validation must reject follow-up"),
        };

        assert_eq!(
            error,
            AgentControlError::DurableMailboxCommit(
                "injected durable validation failure".to_string()
            )
        );
        assert!(
            commit_called.load(std::sync::atomic::Ordering::SeqCst),
            "AwaitingDescendants must delegate durable recovery validation to storage"
        );
        let after = control
            .list_agents(Some(&target.path))
            .expect("snapshot after rejection")
            .into_iter()
            .next()
            .expect("deferred target");
        assert_eq!(
            after, before,
            "failed durable validation must not mutate mailbox, status, or execution ownership"
        );

        let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = control
            .commit_and_enqueue_mail(&root, &target.path, true, || {
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("failed durable attempt must release its capacity reservation");
        assert!(scheduled.is_empty());
    }

    #[test]
    fn released_deferred_projection_is_atomic_under_mailbox_backpressure() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let parent_session_id = SessionId::new();
        let parent = control
            .restore_inactive_child(
                &root,
                "parent",
                parent_session_id,
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("parent");
        let deferred_session_id = SessionId::new();
        let deferred_turn_id = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
                None,
            )
            .expect("deferred");
        for _ in 0..MAX_AGENT_MAILBOX_NOTICES {
            let _ = control
                .commit_and_enqueue_mail(&deferred.path, &parent.path, false, || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                })
                .expect("fill parent mailbox");
        }
        let request_id = OwnerResumeRequestId::from(HistoryItemId::new());
        let mut scheduled = control
            .project_released_deferred_completion(
                &deferred.path,
                deferred_session_id,
                deferred_turn_id,
                &parent.path,
                parent_session_id,
                InactiveAgentStatus::Completed(Some("exact deferred result".to_string())),
                None,
                HistoryItemId::new(),
                None,
                || Ok(Some(request_id)),
            )
            .expect("atomic released projection");

        let deferred = control
            .list_agents(Some(&deferred.path))
            .expect("deferred snapshot")
            .into_iter()
            .next()
            .expect("deferred");
        assert_eq!(
            deferred.status,
            AgentStatus::Completed(Some("exact deferred result".to_string()))
        );
        assert_eq!(scheduled.len(), 1);
        let owner_resume = scheduled.pop().expect("owner resume");
        assert_eq!(owner_resume.path(), &parent.path);
        assert_eq!(owner_resume.owner_resume_request_id(), Some(request_id));
        assert_eq!(
            control
                .list_agents(Some(&parent.path))
                .expect("parent snapshot")
                .into_iter()
                .next()
                .expect("parent")
                .pending_mail_count,
            MAX_AGENT_MAILBOX_NOTICES
        );
    }

    #[test]
    fn released_deferred_projection_dedupes_history() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let parent_session_id = SessionId::new();
        let parent = control
            .restore_inactive_child(
                &root,
                "parent",
                parent_session_id,
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("parent");
        let deferred_session_id = SessionId::new();
        let deferred_turn_id = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
                None,
            )
            .expect("deferred");
        let history_item_id = HistoryItemId::new();
        let _ = control
            .commit_and_enqueue_mail(&deferred.path, &parent.path, false, || {
                Ok(AgentMailCommit {
                    history_item_id,
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("pre-existing projected notice");
        let canonical_request = OwnerResumeRequestId::from(HistoryItemId::new());
        control
            .restore_pending_owner_resume(&parent.path, canonical_request)
            .expect("canonical pending OwnerResume");

        let scheduled = control
            .project_released_deferred_completion(
                &deferred.path,
                deferred_session_id,
                deferred_turn_id,
                &parent.path,
                parent_session_id,
                InactiveAgentStatus::Completed(None),
                None,
                history_item_id,
                None,
                || Ok(Some(canonical_request)),
            )
            .expect("deduplicated projection");
        assert_eq!(scheduled.len(), 1);
        assert_eq!(
            control
                .mailbox_history_item_ids(&parent.path)
                .expect("parent mailbox"),
            vec![history_item_id]
        );
    }

    #[test]
    fn released_deferred_effect_is_idempotent_and_cannot_overwrite_d2() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let parent_session_id = SessionId::new();
        let parent = control
            .restore_inactive_child(
                &AgentPath::root(),
                "parent",
                parent_session_id,
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("parent");
        let deferred_session_id = SessionId::new();
        let d1 = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(d1),
                None,
            )
            .expect("D1 deferred child");
        let history_item_id = HistoryItemId::new();
        for _ in 0..2 {
            assert!(
                control
                    .project_released_deferred_completion(
                        &deferred.path,
                        deferred_session_id,
                        d1,
                        &parent.path,
                        parent_session_id,
                        InactiveAgentStatus::Completed(Some("D1 result".to_string())),
                        None,
                        history_item_id,
                        None,
                        || Ok(None),
                    )
                    .expect("replay D1 effect")
                    .is_empty()
            );
        }
        assert_eq!(
            control
                .mailbox_history_item_ids(&parent.path)
                .expect("deduplicated parent mail"),
            vec![history_item_id]
        );

        let d2_trigger = HistoryItemId::new();
        let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = control
            .commit_and_enqueue_mail(&parent.path, &deferred.path, true, || {
                Ok(AgentMailCommit {
                    history_item_id: d2_trigger,
                    schedule_turn: true,
                    owner_resume_request_id: None,
                })
            })
            .expect("schedule D2");
        let d2_execution = scheduled.pop().expect("D2 execution");
        let d2 = TurnId::new();
        control
            .mark_execution_admitted(
                &d2_execution.scope(),
                AgentExecutionWakeCause::ExplicitTask(d2_trigger),
                d2,
                None,
                || Ok(None),
            )
            .expect("admit D2");

        assert!(
            control
                .project_released_deferred_completion(
                    &deferred.path,
                    deferred_session_id,
                    d1,
                    &parent.path,
                    parent_session_id,
                    InactiveAgentStatus::Completed(Some("D1 result".to_string())),
                    None,
                    history_item_id,
                    None,
                    || Ok(None),
                )
                .expect("late D1 replay during D2")
                .is_empty()
        );
        let d2_snapshot = control
            .list_agents(Some(&deferred.path))
            .expect("D2 snapshot")
            .into_iter()
            .next()
            .expect("D2 child");
        assert_eq!(d2_snapshot.status, AgentStatus::Running);
        assert!(d2_snapshot.is_active);
        assert_eq!(
            control
                .mailbox_history_item_ids(&parent.path)
                .expect("parent mail after D1 replay"),
            vec![history_item_id]
        );
        drop(d2_execution);
    }

    #[test]
    fn released_deferred_projection_reconciles_stale_local_owner_to_durable_current_owner() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let parent_session_id = SessionId::new();
        let parent = control
            .restore_inactive_child(
                &root,
                "parent",
                parent_session_id,
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("parent");
        let deferred_session_id = SessionId::new();
        let deferred_turn_id = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
                None,
            )
            .expect("deferred");
        let stale_request = OwnerResumeRequestId::from(HistoryItemId::new());
        let newer_request = OwnerResumeRequestId::from(HistoryItemId::new());
        control
            .restore_pending_owner_resume(&parent.path, stale_request)
            .expect("stale local OwnerResume");

        let mut scheduled = control
            .project_released_deferred_completion(
                &deferred.path,
                deferred_session_id,
                deferred_turn_id,
                &parent.path,
                parent_session_id,
                InactiveAgentStatus::Completed(Some("exact child result".to_string())),
                Some("child terminal persisted".to_string()),
                HistoryItemId::new(),
                None,
                || Ok(Some(newer_request)),
            )
            .expect("authoritative current parent wake");

        assert_eq!(
            control.status(&deferred.path).expect("released child"),
            AgentStatus::Completed(Some("exact child result".to_string()))
        );
        let child = control
            .list_agents(Some(&deferred.path))
            .expect("child projection")
            .into_iter()
            .next()
            .expect("child");
        assert_eq!(
            child.last_activity.as_deref(),
            Some("child terminal persisted")
        );
        assert_eq!(
            control
                .mailbox_history_item_ids(&parent.path)
                .expect("parent mailbox")
                .len(),
            1,
            "the informational child result is independent of OwnerResume generation"
        );
        assert_eq!(scheduled.len(), 1);
        let continuation = scheduled.pop().expect("newer continuation");
        assert_eq!(continuation.path(), &parent.path);
        assert_eq!(
            continuation.owner_resume_request_id(),
            Some(newer_request),
            "the durable current owner must replace stale local scheduler state"
        );
    }

    #[test]
    fn released_deferred_projection_clears_stale_local_owner_when_durable_current_is_none() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let parent_session_id = SessionId::new();
        let parent = control
            .restore_inactive_child(
                &AgentPath::root(),
                "parent",
                parent_session_id,
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("parent");
        let deferred_session_id = SessionId::new();
        let deferred_turn_id = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
                None,
            )
            .expect("deferred");
        control
            .restore_pending_owner_resume(
                &parent.path,
                OwnerResumeRequestId::from(HistoryItemId::new()),
            )
            .expect("stale local OwnerResume");

        assert!(
            control
                .project_released_deferred_completion(
                    &deferred.path,
                    deferred_session_id,
                    deferred_turn_id,
                    &parent.path,
                    parent_session_id,
                    InactiveAgentStatus::Completed(None),
                    None,
                    HistoryItemId::new(),
                    None,
                    || Ok(None),
                )
                .expect("authoritative empty current owner")
                .is_empty()
        );
        assert!(
            control
                .schedule_pending_triggered_executions()
                .expect("scheduler after stale clear")
                .is_empty()
        );
    }

    #[test]
    fn durable_release_promotes_explicit_while_live_parent_snapshot_still_runs() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let parent_session_id = SessionId::new();
        let (parent, parent_execution) = control
            .register_child(
                &root,
                "parent",
                parent_session_id,
                Some("finishing completed-early parent".to_string()),
            )
            .expect("active parent");
        let parent_trigger = HistoryItemId::new();
        let parent_turn_id = TurnId::new();
        let parent_execution = parent_execution
            .try_bind_trigger_history_item_id(parent_trigger)
            .map_err(drop)
            .expect("bind active parent trigger");
        control
            .mark_execution_admitted(
                &parent_execution.scope(),
                AgentExecutionWakeCause::ExplicitTask(parent_trigger),
                parent_turn_id,
                Some("stale running parent snapshot".to_string()),
                || Ok(None),
            )
            .expect("bind active parent generation");
        let deferred_session_id = SessionId::new();
        let deferred_turn_id = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
                None,
            )
            .expect("deferred child");
        let explicit_id = HistoryItemId::new();
        control
            .restore_pending_mail(&parent.path, explicit_id, false)
            .expect("dormant explicit parent task");

        assert!(
            control
                .project_released_deferred_completion(
                    &deferred.path,
                    deferred_session_id,
                    deferred_turn_id,
                    &parent.path,
                    parent_session_id,
                    InactiveAgentStatus::Completed(None),
                    None,
                    HistoryItemId::new(),
                    Some(parent_turn_id),
                    || Ok(None),
                )
                .expect("project exact durable release")
                .is_empty(),
            "the active parent must retain its current execution"
        );
        let mut scheduled = control
            .complete_execution(
                parent_execution,
                InactiveAgentStatus::AwaitingDescendants(parent_turn_id),
                None,
            )
            .expect("publish eventual completed-early parent state");
        assert_eq!(scheduled.len(), 1);
        let explicit = scheduled.pop().expect("promoted explicit continuation");
        assert_eq!(explicit.path(), &parent.path);
        assert_eq!(explicit.trigger_history_item_id(), Some(explicit_id));
        assert_eq!(explicit.owner_resume_request_id(), None);
    }

    #[test]
    fn released_deferred_projection_promotes_explicit_parent_without_owner_resume_identity_at_capacity()
     {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let parent_session_id = SessionId::new();
        let parent_deferred_turn_id = TurnId::new();
        let parent = control
            .restore_inactive_child(
                &root,
                "parent",
                parent_session_id,
                InactiveAgentStatus::AwaitingDescendants(parent_deferred_turn_id),
                None,
            )
            .expect("awaiting parent");
        let deferred_session_id = SessionId::new();
        let deferred_turn_id = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
                None,
            )
            .expect("deferred child");
        let explicit_id = HistoryItemId::new();
        control
            .restore_pending_mail(&parent.path, explicit_id, false)
            .expect("dormant explicit parent task");
        for _ in 1..MAX_AGENT_MAILBOX_NOTICES {
            let _ = control
                .commit_and_enqueue_mail(&deferred.path, &parent.path, false, || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                })
                .expect("fill awaiting parent mailbox");
        }

        let mut scheduled = control
            .project_released_deferred_completion(
                &deferred.path,
                deferred_session_id,
                deferred_turn_id,
                &parent.path,
                parent_session_id,
                InactiveAgentStatus::Completed(Some("deferred result".to_string())),
                None,
                HistoryItemId::new(),
                Some(parent_deferred_turn_id),
                || Ok(None),
            )
            .expect("release deferred owner with explicit precedence");

        assert_eq!(scheduled.len(), 1);
        let parent_execution = scheduled.pop().expect("promoted explicit parent");
        assert_eq!(parent_execution.path(), &parent.path);
        assert_eq!(
            parent_execution.trigger_history_item_id(),
            Some(explicit_id)
        );
        assert_eq!(parent_execution.owner_resume_request_id(), None);
        assert_eq!(
            control
                .list_agents(Some(&parent.path))
                .expect("full parent mailbox")
                .into_iter()
                .next()
                .expect("parent")
                .pending_mail_count,
            MAX_AGENT_MAILBOX_NOTICES
        );
        assert_eq!(
            control.status(&deferred.path).expect("released deferred"),
            AgentStatus::Completed(Some("deferred result".to_string()))
        );
    }

    #[test]
    fn released_deferred_projection_finalizes_when_terminal_parent_mailbox_is_full() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let parent_session_id = SessionId::new();
        let parent = control
            .restore_inactive_child(
                &root,
                "parent",
                parent_session_id,
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("terminal parent");
        let deferred_session_id = SessionId::new();
        let deferred_turn_id = TurnId::new();
        let deferred = control
            .restore_inactive_child(
                &parent.path,
                "deferred",
                deferred_session_id,
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
                None,
            )
            .expect("deferred child");
        for _ in 0..MAX_AGENT_MAILBOX_NOTICES {
            let _ = control
                .commit_and_enqueue_mail(&deferred.path, &parent.path, false, || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                })
                .expect("fill terminal parent mailbox");
        }

        let scheduled = control
            .project_released_deferred_completion(
                &deferred.path,
                deferred_session_id,
                deferred_turn_id,
                &parent.path,
                parent_session_id,
                InactiveAgentStatus::Completed(Some("released result".to_string())),
                None,
                HistoryItemId::new(),
                None,
                || Ok(None),
            )
            .expect("durable release cannot be gated by process-local mailbox capacity");

        assert!(scheduled.is_empty());
        assert_eq!(
            control.status(&deferred.path).expect("released deferred"),
            AgentStatus::Completed(Some("released result".to_string()))
        );
        assert_eq!(
            control
                .list_agents(Some(&parent.path))
                .expect("full terminal parent")
                .into_iter()
                .next()
                .expect("parent")
                .pending_mail_count,
            MAX_AGENT_MAILBOX_NOTICES
        );
    }

    #[test]
    fn stale_deferred_release_cannot_promote_a_later_generation() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let (owner, owner_execution) = control
            .register_child(&root, "owner", SessionId::new(), None)
            .expect("active owner");
        let d1_trigger = HistoryItemId::new();
        let d1 = TurnId::new();
        let owner_execution = owner_execution
            .try_bind_trigger_history_item_id(d1_trigger)
            .map_err(drop)
            .expect("bind D1 trigger");
        control
            .mark_execution_admitted(
                &owner_execution.scope(),
                AgentExecutionWakeCause::ExplicitTask(d1_trigger),
                d1,
                None,
                || Ok(None),
            )
            .expect("admit D1");
        let e1 = HistoryItemId::new();
        control
            .restore_pending_mail(&owner.path, e1, false)
            .expect("dormant E1");

        let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = control
            .commit_and_enqueue_completion_handoff(&root, &owner.path, Some(d1), || {
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("latch D1 release");
        assert!(scheduled.is_empty());
        let mut scheduled = control
            .complete_execution(
                owner_execution,
                InactiveAgentStatus::AwaitingDescendants(d1),
                None,
            )
            .expect("publish D1 awaiting state");
        assert_eq!(scheduled.len(), 1);
        let d2_execution = scheduled.pop().expect("E1 continuation");
        assert_eq!(d2_execution.trigger_history_item_id(), Some(e1));

        let d2 = TurnId::new();
        control
            .mark_execution_admitted(
                &d2_execution.scope(),
                AgentExecutionWakeCause::ExplicitTask(e1),
                d2,
                None,
                || Ok(None),
            )
            .expect("admit D2");
        control
            .drain_mailbox(&owner.path)
            .expect("D2 claimed its delivered E1 input");
        let e2 = HistoryItemId::new();
        control
            .restore_pending_mail(&owner.path, e2, false)
            .expect("dormant E2");

        for _ in 0..2 {
            let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = control
                .commit_and_enqueue_completion_handoff(&root, &owner.path, Some(d1), || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                })
                .expect("delayed D1 release");
            assert!(scheduled.is_empty());
        }
        assert!(
            control
                .complete_execution(
                    d2_execution,
                    InactiveAgentStatus::AwaitingDescendants(d2),
                    None,
                )
                .expect("publish D2 awaiting state")
                .is_empty(),
            "a delayed D1 release must not promote E2"
        );
        assert!(
            !control
                .mailbox_has_ready_trigger_turn(&owner.path)
                .expect("E2 readiness")
        );

        let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = control
            .commit_and_enqueue_completion_handoff(&root, &owner.path, Some(d2), || {
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("exact D2 release");
        assert_eq!(scheduled.len(), 1);
        let e2_execution = scheduled.pop().expect("E2 execution");
        assert_eq!(e2_execution.trigger_history_item_id(), Some(e2));
        let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = control
            .commit_and_enqueue_completion_handoff(&root, &owner.path, Some(d2), || {
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("repeated D2 release");
        assert!(
            scheduled.is_empty(),
            "one exact release must reserve at most one continuation"
        );
        drop(e2_execution);
    }

    #[test]
    fn current_owner_resume_reconciliation_clears_r1_and_replaces_it_with_r2() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let child = control
            .restore_inactive_child(
                &AgentPath::root(),
                "owner",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("retained owner");
        let r1 = OwnerResumeRequestId::from(HistoryItemId::new());
        let r2 = OwnerResumeRequestId::from(HistoryItemId::new());
        control
            .restore_pending_owner_resume(&child.path, r1)
            .expect("restore R1");
        assert!(
            control
                .restore_current_owner_resume(&child.path, || Ok(None))
                .expect("reconcile R1 to none")
                .is_empty()
        );
        assert!(
            control
                .schedule_pending_triggered_executions()
                .expect("scheduler after R1 clear")
                .is_empty()
        );

        control
            .restore_pending_owner_resume(&child.path, r1)
            .expect("restore R1 again");
        let mut scheduled = control
            .restore_current_owner_resume(&child.path, || Ok(Some(r2)))
            .expect("replace R1 with R2");
        assert_eq!(scheduled.len(), 1);
        assert_eq!(
            scheduled
                .pop()
                .expect("R2 execution")
                .owner_resume_request_id(),
            Some(r2)
        );
    }

    #[test]
    fn admission_reconciliation_preserves_a_newer_owner_resume() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let child = control
            .restore_inactive_child(
                &AgentPath::root(),
                "owner",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("retained owner");
        let r1 = OwnerResumeRequestId::from(HistoryItemId::new());
        let r2 = OwnerResumeRequestId::from(HistoryItemId::new());
        control
            .restore_pending_owner_resume(&child.path, r1)
            .expect("restore R1");
        let mut scheduled = control
            .schedule_pending_triggered_executions()
            .expect("schedule R1");
        let execution = scheduled.pop().expect("R1 execution");
        control
            .mark_execution_admitted(
                &execution.scope(),
                AgentExecutionWakeCause::OwnerResume(r1),
                TurnId::new(),
                None,
                || Ok(Some(r2)),
            )
            .expect("admit R1 and reconcile current R2");
        let mut scheduled = control
            .complete_execution(execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete R1 turn");
        assert_eq!(scheduled.len(), 1);
        assert_eq!(
            scheduled
                .pop()
                .expect("R2 execution")
                .owner_resume_request_id(),
            Some(r2)
        );
    }

    #[test]
    fn repeated_completion_projection_deduplicates_local_notice_identity() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let owner = control
            .restore_inactive_child(
                &AgentPath::root(),
                "owner",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("retained owner");
        let history_item_id = HistoryItemId::new();
        for _ in 0..2 {
            let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = control
                .commit_and_enqueue_completion_handoff(
                    &AgentPath::root(),
                    &owner.path,
                    None,
                    || {
                        Ok(AgentMailCommit {
                            history_item_id,
                            schedule_turn: false,
                            owner_resume_request_id: None,
                        })
                    },
                )
                .expect("idempotent completion projection");
            assert!(scheduled.is_empty());
        }
        assert_eq!(
            control
                .mailbox_history_item_ids(&owner.path)
                .expect("owner mailbox"),
            vec![history_item_id]
        );
    }

    #[test]
    fn restart_scheduler_runs_nested_explicit_target_without_resuming_parent() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root owner");
        let root = AgentPath::root();
        let parent = control
            .restore_inactive_child(
                &root,
                "parent",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("restored parent");
        let grandchild = control
            .restore_inactive_child(
                &parent.path,
                "grandchild",
                SessionId::new(),
                InactiveAgentStatus::PendingInit,
                None,
            )
            .expect("restored grandchild");
        let grandchild_trigger = HistoryItemId::new();
        control
            .restore_pending_mail(&grandchild.path, grandchild_trigger, true)
            .expect("grandchild trigger");

        let mut scheduled = control
            .schedule_pending_triggered_executions()
            .expect("restart target scheduling pass");
        assert_eq!(scheduled.len(), 1);
        let grandchild_execution = scheduled.pop().expect("grandchild execution");
        assert_eq!(grandchild_execution.path(), &grandchild.path);
        assert_eq!(
            grandchild_execution.trigger_history_item_id(),
            Some(grandchild_trigger)
        );
        assert!(
            !control
                .list_agents(Some(&parent.path))
                .expect("parent snapshot")
                .into_iter()
                .next()
                .expect("parent")
                .is_active
        );
    }

    #[test]
    fn live_nested_trigger_schedules_only_the_direct_target() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root");
        let root = AgentPath::root();
        let parent = control
            .restore_inactive_child(
                &root,
                "parent",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("retained parent");
        let child = control
            .restore_inactive_child(
                &parent.path,
                "child",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("retained child");
        let trigger = HistoryItemId::new();
        let delivery = control
            .commit_and_enqueue_mail(&root, &child.path, true, || {
                Ok(AgentMailCommit {
                    history_item_id: trigger,
                    schedule_turn: true,
                    owner_resume_request_id: None,
                })
            })
            .expect("durable nested delivery");
        let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = delivery;
        assert_eq!(scheduled.len(), 1);
        let child_execution = scheduled.pop().expect("direct target execution");
        assert_eq!(child_execution.path(), &child.path);
        assert_eq!(child_execution.trigger_history_item_id(), Some(trigger));
        assert!(
            !control
                .list_agents(Some(&parent.path))
                .expect("parent snapshot")
                .into_iter()
                .next()
                .expect("parent")
                .is_active,
            "an inactive ancestor is not resumed for a direct nested follow-up"
        );
    }

    #[test]
    fn explicit_admission_coalesces_pending_owner_resume_without_second_turn() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root");
        let child = control
            .restore_inactive_child(
                &AgentPath::root(),
                "child",
                SessionId::new(),
                InactiveAgentStatus::PendingInit,
                None,
            )
            .expect("retained child");
        let owner_resume = OwnerResumeRequestId::from(HistoryItemId::new());
        let explicit_trigger = HistoryItemId::new();
        control
            .restore_pending_owner_resume(&child.path, owner_resume)
            .expect("pending OwnerResume");
        control
            .restore_pending_mail(&child.path, explicit_trigger, true)
            .expect("explicit trigger");

        let mut scheduled = control
            .schedule_pending_triggered_executions()
            .expect("explicit precedence scheduling");
        assert_eq!(scheduled.len(), 1);
        let execution = scheduled.pop().expect("explicit execution");
        assert_eq!(execution.trigger_history_item_id(), Some(explicit_trigger));
        assert!(
            control
                .mark_execution_admitted(
                    &execution.scope(),
                    AgentExecutionWakeCause::ExplicitTask(explicit_trigger),
                    TurnId::new(),
                    Some("explicit task admitted".to_string()),
                    || Ok(None),
                )
                .expect("admit explicit task")
                .is_empty()
        );
        control
            .drain_mailbox(&child.path)
            .expect("claim explicit trigger");
        assert!(
            control
                .complete_execution(execution, InactiveAgentStatus::Completed(None), None)
                .expect("complete explicit task")
                .is_empty(),
            "coalesced OwnerResume must not schedule a redundant turn"
        );
        assert!(
            control
                .schedule_pending_triggered_executions()
                .expect("no residual schedulable work")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn mailbox_preserves_fifo_order_and_notifies_by_generation() {
        let (control, root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
        root_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("admitted root");
        let root = AgentPath::root();
        let (child, _child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let mut activity = control.subscribe_mailbox(&root).expect("subscription");
        let first_id = HistoryItemId::new();
        let second_id = HistoryItemId::new();

        assert_eq!(
            match control
                .commit_and_enqueue_mail(&child.path, &root, false, || {
                    Ok(AgentMailCommit {
                        history_item_id: first_id,
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                })
                .expect("first mail")
            {
                AgentMailDeliveryOutcome::Enqueued { generation, .. } => generation,
            },
            1
        );
        assert_eq!(
            match control
                .commit_and_enqueue_mail(&child.path, &root, true, || {
                    Ok(AgentMailCommit {
                        history_item_id: second_id,
                        schedule_turn: true,
                        owner_resume_request_id: None,
                    })
                })
                .expect("second mail")
            {
                AgentMailDeliveryOutcome::Enqueued { generation, .. } => generation,
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
            .commit_and_enqueue_mail(&child.path, &root, false, || {
                Ok(AgentMailCommit {
                    history_item_id,
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("durable mail");
        let AgentMailDeliveryOutcome::Enqueued {
            generation,
            scheduled,
        } = outcome;
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
    fn pending_trigger_terminal_fence_preserves_informational_and_later_trigger_notices() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let (informational_id, _) =
            enqueue_test_notice(&control, &root, &child.path, false).expect("informational notice");
        let (stale_trigger_id, _) =
            enqueue_test_notice(&control, &root, &child.path, true).expect("stale trigger");
        let child_execution =
            match child_execution.try_bind_trigger_history_item_id(stale_trigger_id) {
                Ok(lease) => lease,
                Err(_) => panic!("initial child lease must accept its durable trigger identity"),
            };
        let (preexisting_followup_id, _) = enqueue_test_notice(&control, &root, &child.path, true)
            .expect("pre-existing later trigger");

        let (terminal_entered_tx, terminal_entered_rx) = std::sync::mpsc::channel();
        let (release_terminal_tx, release_terminal_rx) = std::sync::mpsc::channel();
        let terminal_control = control.clone();
        let terminal = std::thread::spawn(move || {
            let committed =
                terminal_control.commit_pending_trigger_terminal(&child_execution, None, || {
                    terminal_entered_tx
                        .send(())
                        .expect("terminal commit entered signal");
                    release_terminal_rx
                        .recv()
                        .expect("release terminal durable commit");
                    Ok(PendingTriggerTerminalCommit::Applied("durable terminal"))
                });
            (committed, child_execution)
        });
        terminal_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("terminal reached durable commit");

        let followup_id = HistoryItemId::new();
        let (followup_commit_entered_tx, followup_commit_entered_rx) = std::sync::mpsc::channel();
        let followup_control = control.clone();
        let followup_root = root.clone();
        let followup_child = child.path.clone();
        let followup = std::thread::spawn(move || {
            followup_control.commit_and_enqueue_mail(&followup_root, &followup_child, true, || {
                followup_commit_entered_tx
                    .send(())
                    .expect("follow-up commit entered signal");
                Ok(AgentMailCommit {
                    history_item_id: followup_id,
                    schedule_turn: true,
                    owner_resume_request_id: None,
                })
            })
        });
        assert!(
            followup_commit_entered_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "a later delivery must wait until durable terminal commit and stale-trigger purge finish"
        );

        release_terminal_tx
            .send(())
            .expect("release terminal commit");
        let (terminal_result, child_execution) =
            terminal.join().expect("terminal thread completion");
        assert_eq!(
            terminal_result.expect("durable terminal fence"),
            PendingTriggerTerminalCommit::Applied("durable terminal")
        );
        followup_commit_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("follow-up commit proceeds after terminal fence");
        let outcome = followup
            .join()
            .expect("follow-up thread completion")
            .expect("follow-up delivery");
        assert!(matches!(
            outcome,
            AgentMailDeliveryOutcome::Enqueued { ref scheduled, .. } if scheduled.is_empty()
        ));

        let notices = control
            .drain_mailbox(&child.path)
            .expect("post-terminal mailbox");
        assert_eq!(
            notices
                .iter()
                .map(|notice| (notice.history_item_id, notice.trigger_turn))
                .collect::<Vec<_>>(),
            vec![
                (informational_id, false),
                (preexisting_followup_id, true),
                (followup_id, true),
            ],
            "the terminal may purge only the exact settled trigger notice"
        );
        assert!(
            notices
                .iter()
                .all(|notice| notice.history_item_id != stale_trigger_id)
        );
        drop(child_execution);
    }

    #[test]
    fn blocked_trigger_terminal_atomically_returns_to_exact_awaiting_generation() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let (exact_trigger, _) =
            enqueue_test_notice(&control, &root, &child.path, true).expect("exact trigger");
        let child_execution = child_execution
            .try_bind_trigger_history_item_id(exact_trigger)
            .map_err(drop)
            .expect("bind exact trigger");
        let (later_trigger, _) =
            enqueue_test_notice(&control, &root, &child.path, true).expect("later trigger");
        let deferred_turn_id = TurnId::new();

        assert_eq!(
            control
                .commit_pending_trigger_terminal(
                    &child_execution,
                    Some("waiting for exact descendants".to_string()),
                    || {
                        Ok(
                            PendingTriggerTerminalCommit::<()>::BlockedByPendingDeferredCompletion {
                                deferred_turn_id,
                            },
                        )
                    },
                )
                .expect("blocked durable settlement"),
            PendingTriggerTerminalCommit::BlockedByPendingDeferredCompletion { deferred_turn_id }
        );
        let snapshot = control
            .list_agents(Some(&child.path))
            .expect("blocked child snapshot")
            .into_iter()
            .next()
            .expect("blocked child");
        assert_eq!(snapshot.status, AgentStatus::AwaitingDescendants);
        assert!(!snapshot.is_active);
        assert_eq!(snapshot.pending_mail_count, 2);
        assert!(
            !control
                .mailbox_has_ready_trigger_turn(&child.path)
                .expect("all trigger notices are dormant")
        );
        assert_eq!(
            control
                .mailbox_history_item_ids(&child.path)
                .expect("retained exact identities"),
            vec![exact_trigger, later_trigger]
        );

        let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = control
            .commit_and_enqueue_completion_handoff(
                &root,
                &child.path,
                Some(deferred_turn_id),
                || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: false,
                        owner_resume_request_id: None,
                    })
                },
            )
            .expect("exact deferred release");
        assert_eq!(scheduled.len(), 1);
        assert_eq!(
            scheduled
                .pop()
                .expect("released continuation")
                .trigger_history_item_id(),
            Some(later_trigger),
            "the newest retained explicit trigger owns the one continuation"
        );
        drop(child_execution);
    }

    #[test]
    fn cancelled_active_generation_rejects_its_late_release_latch() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let trigger = HistoryItemId::new();
        let turn_id = TurnId::new();
        let child_execution = child_execution
            .try_bind_trigger_history_item_id(trigger)
            .map_err(drop)
            .expect("bind trigger");
        control
            .mark_execution_admitted(
                &child_execution.scope(),
                AgentExecutionWakeCause::ExplicitTask(trigger),
                turn_id,
                None,
                || Ok(None),
            )
            .expect("admit active generation");
        let dormant = HistoryItemId::new();
        control
            .restore_pending_mail(&child.path, dormant, false)
            .expect("dormant continuation");
        control
            .cancel_agent(&child.path)
            .expect("cancel active child");
        let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = control
            .commit_and_enqueue_completion_handoff(&root, &child.path, Some(turn_id), || {
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("late cancelled-generation release");
        assert!(scheduled.is_empty());
        assert!(
            control
                .complete_execution(
                    child_execution,
                    InactiveAgentStatus::AwaitingDescendants(turn_id),
                    None,
                )
                .is_err(),
            "cancel clears active generation ownership before a late Awaiting publish"
        );
        assert!(
            !control
                .mailbox_has_ready_trigger_turn(&child.path)
                .expect("cancelled dormant trigger")
        );
    }

    #[test]
    fn claimed_trigger_retirement_preserves_mail_and_schedules_the_newer_trigger() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let (informational_id, _) =
            enqueue_test_notice(&control, &root, &child.path, false).expect("informational notice");
        let (claimed_trigger_id, _) =
            enqueue_test_notice(&control, &root, &child.path, true).expect("claimed trigger");
        let child_execution =
            match child_execution.try_bind_trigger_history_item_id(claimed_trigger_id) {
                Ok(lease) => lease,
                Err(_) => panic!("initial child lease must accept its durable trigger identity"),
            };
        let (newer_trigger_id, newer_outcome) =
            enqueue_test_notice(&control, &root, &child.path, true).expect("newer trigger");
        assert!(matches!(
            newer_outcome,
            AgentMailDeliveryOutcome::Enqueued { ref scheduled, .. } if scheduled.is_empty()
        ));

        let mut scheduled = control
            .retire_resolved_wake_execution(
                child_execution,
                Some("exact durable wake was already owned or resolved".to_string()),
            )
            .expect("retire resolved wake");
        assert_eq!(scheduled.len(), 1);
        let next_execution = scheduled.pop().expect("newer trigger execution");
        assert_eq!(
            next_execution.trigger_history_item_id(),
            Some(newer_trigger_id)
        );

        let notices = control
            .drain_mailbox(&child.path)
            .expect("retained mailbox");
        assert_eq!(
            notices
                .iter()
                .map(|notice| (notice.history_item_id, notice.trigger_turn))
                .collect::<Vec<_>>(),
            vec![(informational_id, false), (newer_trigger_id, true)]
        );
        assert!(
            notices
                .iter()
                .all(|notice| notice.history_item_id != claimed_trigger_id)
        );
        assert!(
            control
                .complete_execution(next_execution, InactiveAgentStatus::Completed(None), None,)
                .expect("complete newer trigger execution")
                .is_empty()
        );
    }

    #[test]
    fn unsettled_trigger_release_preserves_mail_and_remains_reschedulable() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let (informational_id, _) =
            enqueue_test_notice(&control, &root, &child.path, false).expect("informational notice");
        let (trigger_id, _) =
            enqueue_test_notice(&control, &root, &child.path, true).expect("trigger");
        let child_execution = match child_execution.try_bind_trigger_history_item_id(trigger_id) {
            Ok(lease) => lease,
            Err(_) => panic!("initial child lease must accept its durable trigger identity"),
        };

        control
            .release_unsettled_trigger_execution(
                child_execution,
                Some("durable settlement failed".to_string()),
            )
            .expect("release unsettled trigger");
        let child_snapshot = control
            .list_agents(Some(&child.path))
            .expect("child snapshot")
            .into_iter()
            .next()
            .expect("child");
        assert_eq!(child_snapshot.status, AgentStatus::PendingInit);
        assert!(!child_snapshot.is_active);
        assert_eq!(child_snapshot.pending_mail_count, 2);

        let mut scheduled = control
            .schedule_pending_triggered_executions()
            .expect("retry scheduler");
        assert_eq!(scheduled.len(), 1);
        let retry_execution = scheduled.pop().expect("retry execution");
        assert_eq!(retry_execution.trigger_history_item_id(), Some(trigger_id));
        let notices = control
            .drain_mailbox(&child.path)
            .expect("retained mailbox");
        assert_eq!(
            notices
                .iter()
                .map(|notice| (notice.history_item_id, notice.trigger_turn))
                .collect::<Vec<_>>(),
            vec![(informational_id, false), (trigger_id, true)]
        );
        assert!(
            control
                .complete_execution(retry_execution, InactiveAgentStatus::Completed(None), None,)
                .expect("complete retry execution")
                .is_empty()
        );
    }

    #[test]
    fn trigger_not_delivered_into_a_running_turn_schedules_one_followup_turn() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (child, child_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        child_execution
            .set_status(ActiveAgentStatus::Running)
            .expect("running child");

        let followup_id = HistoryItemId::new();
        let outcome = control
            .commit_and_enqueue_mail(&root, &child.path, true, || {
                Ok(AgentMailCommit {
                    history_item_id: followup_id,
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("turn-scoped follow-up");
        let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = outcome;
        assert!(scheduled.is_empty());
        assert!(
            control
                .mailbox_has_trigger_turn(&child.path)
                .expect("original requested trigger")
        );
        assert!(
            !control
                .mailbox_has_ready_trigger_turn(&child.path)
                .expect("no ready continuation trigger")
        );
        let mut scheduled_after_completion = control
            .complete_execution(child_execution, InactiveAgentStatus::Completed(None), None)
            .expect("complete current child turn");
        assert_eq!(
            scheduled_after_completion.len(),
            1,
            "mail that was accepted but never sampled at a safe boundary must survive into one follow-up turn"
        );
        let followup = scheduled_after_completion
            .pop()
            .expect("one follow-up execution");
        assert_eq!(
            followup.trigger_history_item_id(),
            Some(followup_id),
            "the follow-up execution must retain the durable mailbox identity"
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
            Ok(AgentMailCommit {
                history_item_id: HistoryItemId::new(),
                schedule_turn: false,
                owner_resume_request_id: None,
            })
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
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
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
    fn tree_stop_during_durable_commit_retains_canonical_trigger_without_scheduling() {
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
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: true,
                    owner_resume_request_id: None,
                })
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
        assert!(matches!(
            outcome,
            AgentMailDeliveryOutcome::Enqueued { ref scheduled, .. } if scheduled.is_empty()
        ));
        assert!(
            control
                .mailbox_has_trigger_turn(&child.path)
                .expect("trigger state")
        );
        let child = control
            .list_agents(Some(&child.path))
            .expect("child snapshot")
            .into_iter()
            .next()
            .expect("child");
        assert_eq!(child.pending_mail_count, 1);
        assert!(!child.is_active);
    }

    #[test]
    fn session_trigger_committed_after_child_terminal_starts_a_new_turn() {
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
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: true,
                    owner_resume_request_id: None,
                })
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

        assert!(matches!(
            outcome,
            AgentMailDeliveryOutcome::Enqueued { ref scheduled, .. } if scheduled.len() == 1
        ));
        assert!(
            control
                .mailbox_has_trigger_turn(&child.path)
                .expect("trigger state")
        );
        let child = control
            .list_agents(Some(&child.path))
            .expect("child snapshot")
            .into_iter()
            .next()
            .expect("child");
        assert_eq!(child.pending_mail_count, 1);
        assert!(child.is_active);
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
    fn durable_terminal_cancel_is_exact_for_child_and_root() {
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
        drop(child_execution);
        let restarted_child = control
            .try_acquire_execution(&child.path)
            .expect("restart child after exact terminal cancellation");

        control
            .cancel_for_durable_terminal(&root)
            .expect("durable root terminal");
        assert!(root_execution.cancel_token().is_cancelled());
        assert!(!restarted_child.cancel_token().is_cancelled());
        assert!(!control.tree_is_cancelled());
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
                .commit_and_enqueue_mail(&root, &child.path, true, || {
                    Ok(AgentMailCommit {
                        history_item_id: HistoryItemId::new(),
                        schedule_turn: true,
                        owner_resume_request_id: None,
                    })
                })
                .expect("follow-up after converged purges");
            let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = outcome;
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
                schedule_ready: true,
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

    #[test]
    fn durable_mail_rejects_a_replaced_author_execution_before_projection() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let (author, first_author_execution) = control
            .register_child(&root, "author", SessionId::new(), None)
            .expect("author");
        let recipient = control
            .restore_inactive_child(
                &root,
                "recipient",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("recipient");
        let stale_author = first_author_execution.scope();
        drop(first_author_execution);
        let replacement = control
            .try_acquire_execution(&author.path)
            .expect("replacement author execution");
        let durable_commit_called = std::sync::atomic::AtomicBool::new(false);

        let error = match control.commit_and_enqueue_mail_with_capacity(
            &stale_author,
            &author.path,
            &recipient.path,
            true,
            |_| {
                durable_commit_called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: true,
                    owner_resume_request_id: None,
                })
            },
        ) {
            Err(error) => error,
            Ok(_) => panic!("stale author must not commit or project mail"),
        };
        assert!(matches!(
            error,
            AgentControlError::StaleExecution(path) if path == author.path
        ));
        assert!(!durable_commit_called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            control
                .mailbox_history_item_ids(&recipient.path)
                .expect("recipient mailbox")
                .is_empty()
        );
        assert!(!replacement.cancel_token().is_cancelled());
    }

    #[test]
    fn durable_mail_projects_after_the_committed_author_execution_is_replaced() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let (author, author_execution) = control
            .register_child(&root, "author", SessionId::new(), None)
            .expect("author");
        let recipient = control
            .restore_inactive_child(
                &root,
                "recipient",
                SessionId::new(),
                InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("recipient");
        let author_scope = author_execution.scope();
        let replacement = std::sync::Arc::new(std::sync::Mutex::new(None));
        let replacement_slot = replacement.clone();
        let replacement_control = control.clone();
        let author_path = author.path.clone();

        let history_item_id = HistoryItemId::new();
        let outcome = control
            .commit_and_enqueue_mail_with_capacity(
                &author_scope,
                &author.path,
                &recipient.path,
                true,
                move |_| {
                    drop(author_execution);
                    *replacement_slot.lock().expect("replacement slot") = Some(
                        replacement_control
                            .try_acquire_execution(&author_path)
                            .expect("replacement author execution"),
                    );
                    Ok(AgentMailCommit {
                        history_item_id,
                        schedule_turn: true,
                        owner_resume_request_id: None,
                    })
                },
            )
            .expect("durably committed mail must finish recipient projection");
        let AgentMailDeliveryOutcome::Enqueued { .. } = outcome;
        assert_eq!(
            control
                .mailbox_history_item_ids(&recipient.path)
                .expect("recipient mailbox"),
            vec![history_item_id]
        );
        assert!(
            !replacement
                .lock()
                .expect("replacement slot")
                .as_ref()
                .expect("replacement execution")
                .cancel_token()
                .is_cancelled()
        );
    }

    #[test]
    fn captured_interrupt_rejects_a_replacement_at_the_same_path() {
        let (control, caller_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (target, first_target_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let captured = control
            .capture_interrupt_target(&caller_execution.scope(), &target.path)
            .expect("capture first target execution");
        drop(first_target_execution);
        let replacement = control
            .try_acquire_execution(&target.path)
            .expect("replacement target execution");
        let durable_commit_called = std::sync::atomic::AtomicBool::new(false);

        let error = control
            .commit_and_interrupt_captured(&caller_execution.scope(), &captured, || {
                durable_commit_called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .expect_err("captured target marker is stale");
        assert!(matches!(
            error,
            AgentControlError::StaleExecution(path) if path == target.path
        ));
        assert!(!durable_commit_called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!replacement.cancel_token().is_cancelled());
    }

    #[test]
    fn committed_exact_interrupt_wins_over_a_competing_tree_stop() {
        let (control, caller_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (target, target_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let captured = control
            .capture_interrupt_target(&caller_execution.scope(), &target.path)
            .expect("capture target");
        let stop_control = control.clone();
        let start_stop = std::sync::Arc::new(std::sync::Barrier::new(2));
        let stop_barrier = start_stop.clone();
        let stop = std::thread::spawn(move || {
            stop_barrier.wait();
            stop_control.interrupt_tree(TurnInterruptionCause::UserStop)
        });

        control
            .commit_and_interrupt_captured(&caller_execution.scope(), &captured, || {
                start_stop.wait();
                std::thread::yield_now();
                Ok(())
            })
            .expect("exact interruption commit");
        let _ = stop.join().expect("tree stop thread");
        assert_eq!(
            target_execution.run_control().cause(),
            Some(crate::runtime::RunCancellationCause::Interruption(
                TurnInterruptionCause::AgentInterrupted
            ))
        );
    }

    #[test]
    fn interrupt_does_not_commit_activity_after_target_success_commit_begins() {
        let (control, caller_execution) =
            AgentControl::new(SessionId::new(), 2).expect("agent tree");
        let root = AgentPath::root();
        let (target, target_execution) = control
            .register_child(&root, "worker", SessionId::new(), None)
            .expect("worker");
        let captured = control
            .capture_interrupt_target(&caller_execution.scope(), &target.path)
            .expect("capture target");
        let success = target_execution
            .run_control()
            .begin_success_commit()
            .expect("target success reservation");
        let durable_commit_called = std::sync::atomic::AtomicBool::new(false);

        let error = control
            .commit_and_interrupt_captured(&caller_execution.scope(), &captured, || {
                durable_commit_called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .expect_err("success-committing target cannot be interrupted");
        assert!(matches!(
            error,
            AgentControlError::StaleExecution(path) if path == target.path
        ));
        assert!(!durable_commit_called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(success.seal());
    }

    #[test]
    fn hard_abort_candidates_are_only_cancelled_generations_still_holding_a_lease() {
        let (control, _root_execution) =
            AgentControl::new(SessionId::new(), 3).expect("agent tree");
        let root = AgentPath::root();
        let (cancelled, cancelled_execution) = control
            .register_child(&root, "cancelled", SessionId::new(), None)
            .expect("cancelled worker");
        let (running, running_execution) = control
            .register_child(&root, "running", SessionId::new(), None)
            .expect("running worker");

        assert!(
            cancelled_execution
                .run_control()
                .interrupt(TurnInterruptionCause::AgentInterrupted)
        );
        assert_eq!(
            control
                .cancelled_execution_paths()
                .expect("cancelled worker projection"),
            vec![cancelled.path.clone()]
        );

        drop(cancelled_execution);
        assert!(
            control
                .cancelled_execution_paths()
                .expect("released worker projection")
                .is_empty(),
            "a worker that released its exact lease must not be aborted as a later generation"
        );
        assert!(!running_execution.run_control().is_cancelled());
        assert_eq!(running_execution.path(), &running.path);
    }
}
