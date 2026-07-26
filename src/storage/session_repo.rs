use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::config::{AccessMode, ProviderEndpoint};
use crate::error::StorageError;
use crate::protocol::{
    AdditionalContextEntry, CanonicalProtocolSnapshot, ContentPart, HistoryItem, HistoryItemId,
    HistoryItemPayload, HistoryScope, InterAgentCommunication, InterAgentMessageType,
    ModelResponseId, ProtocolPageRequest, RuntimeEvent, RuntimeEventId, RuntimeEventMsg, SteerTurn,
    TurnId, TurnItem, TurnItemId, TurnItemPayload, TurnTerminalOutcome, UserTurn,
    canonical_protocol_snapshot_from_connection, fork_agent_context_in_transaction_for_spawn,
    fork_canonical_items_in_transaction, insert_mailbox_append_order_in_transaction,
    insert_session_owned_event_bundle_in_transaction, latest_protocol_turn_ids_in_transaction,
    project_inter_agent_communication_with_history_item_id, project_protocol_run_event,
    render_inter_agent_message,
};
use crate::runtime::{AgentPath, Clock, SystemClock};
use crate::session::{
    AdmissionId, DurableTurnTerminal, NewSession, ProjectId, RunEvent, SessionForkResult,
    SessionId, SessionModelParameters, SessionRecord, SessionRepository, SessionSettingsPatch,
    SessionSettingsUpdate, SessionSpawnEdge, SessionStatus, SessionTitleUpdate, ThreadGoal,
    ThreadGoalStatus, ToolCallId, ToolCallStatus, validate_session_page_limit,
    validate_thread_goal_objective,
};

pub const RUN_ADMISSION_LEASE_DURATION_MS: i64 = 15_000;
pub const RUN_ADMISSION_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
const EXPIRED_RUN_RECOVERY_REASON: &str =
    "run owner lease expired before the owner acknowledged shutdown";
const AGENT_COMPLETION_MESSAGE_MAX_TOKENS: usize = 1_000;
const AGENT_COMPLETION_MESSAGE_ENVELOPE_TOKEN_RESERVE: usize = 100;
const AGENT_COMPLETION_ERROR_MAX_TOKENS: usize =
    AGENT_COMPLETION_MESSAGE_MAX_TOKENS - AGENT_COMPLETION_MESSAGE_ENVELOPE_TOKEN_RESERVE;
const AGENT_COMPLETION_ERROR_NEXT_ACTION: &str = "This agent's turn failed. If you still need this agent, use the available collaboration tools to give it another task.";
pub(crate) const MAX_DURABLE_AGENT_MAILBOX_MESSAGES: usize = 128;

struct AgentCompletionMessageContract;

impl AgentCompletionMessageContract {
    fn completed_payload(payload: &str) -> String {
        payload.to_string()
    }

    fn failed_payload(error: &str) -> String {
        let error = truncate_agent_completion_middle(error, AGENT_COMPLETION_ERROR_MAX_TOKENS);
        format!("Agent errored: {error}\n\n{AGENT_COMPLETION_ERROR_NEXT_ACTION}")
    }
}

/// Capability proving that a protocol bundle is being inserted from the
/// session repository's atomic state-owner transaction. Its private field
/// prevents generic runtime/projection code from constructing this authority.
pub(crate) struct SessionProtocolWriteAuthority(());

const SESSION_PROTOCOL_WRITE_AUTHORITY: SessionProtocolWriteAuthority =
    SessionProtocolWriteAuthority(());

#[derive(Debug, Clone)]
pub struct PendingToolCallWrite {
    pub id: ToolCallId,
    pub model_call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub protocol_sequence_no: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ModelResponseWrite {
    pub response_id: ModelResponseId,
    pub assistant_text: Option<String>,
    pub assistant_protocol_sequence_no: Option<i64>,
    pub tool_calls: Vec<PendingToolCallWrite>,
}

#[derive(Clone)]
pub struct SqliteSessionRepository {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurableRunAdmission {
    admission_id: AdmissionId,
    turn_id: TurnId,
    lease_expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunningSessionTerminalTarget {
    admission_id: AdmissionId,
    turn_id: TurnId,
}

impl RunningSessionTerminalTarget {
    fn from_admission(admission: DurableRunAdmission) -> Self {
        Self {
            admission_id: admission.admission_id,
            turn_id: admission.turn_id,
        }
    }

    fn matches(self, admission: DurableRunAdmission) -> bool {
        self.admission_id == admission.admission_id && self.turn_id == admission.turn_id
    }

    pub(crate) fn turn_id(self) -> TurnId {
        self.turn_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableSessionStopState {
    Idle,
    Running(RunningSessionTerminalTarget),
    Terminal(SessionStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentTreeStopFence {
    pub(crate) root_session_id: SessionId,
    pub(crate) stopped_session_id: SessionId,
    pub(crate) after_append_position: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentTreeStopFenceCause {
    ApprovalAborted,
    UserStop,
    TreeStopped,
    RootFailed,
}

impl AgentTreeStopFenceCause {
    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "approval_aborted" => Ok(Self::ApprovalAborted),
            "user_stop" => Ok(Self::UserStop),
            "tree_stopped" => Ok(Self::TreeStopped),
            "root_failed" => Ok(Self::RootFailed),
            other => Err(StorageError::Message(format!(
                "unknown durable agent-tree Stop cause `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplicableAgentTreeStopFence {
    root_session_id: SessionId,
    stopped_session_id: SessionId,
    after_append_position: i64,
    cause: AgentTreeStopFenceCause,
}

#[derive(Debug, Clone)]
pub(crate) struct RunningSessionRecoveryCandidate {
    pub session: SessionRecord,
    pub terminal_target: RunningSessionTerminalTarget,
    recovery_depth: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunningSessionRecoveryCursor {
    recovery_depth: i64,
    session_id: SessionId,
}

impl RunningSessionRecoveryCandidate {
    pub(crate) fn cursor(&self) -> RunningSessionRecoveryCursor {
        RunningSessionRecoveryCursor {
            recovery_depth: self.recovery_depth,
            session_id: self.session.id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnContextFork {
    None,
    All,
    Recent(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredAgentCommunication {
    /// Stable durable mailbox identity. Delivery reuses this exact value as the
    /// canonical history item identity.
    pub history_item_id: HistoryItemId,
    pub schedule_turn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveredAgentMailboxPage {
    pub history_item_ids: Vec<HistoryItemId>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveredTurnSteerPage {
    pub history_item_ids: Vec<HistoryItemId>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentMailboxDeliverySelector {
    AllPending,
    RequiredChildResultsOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredAgentCompletionHandoff {
    pub child_session_id: SessionId,
    pub child_turn_id: TurnId,
    pub parent_session_id: SessionId,
    pub parent_agent_path: AgentPath,
    /// Stable durable mailbox identity and eventual canonical history identity.
    pub history_item_id: HistoryItemId,
    /// Exact deferred-owner generation superseded by this child terminal.
    ///
    /// A reusable session/path or a boolean release flag cannot distinguish a delayed handoff for
    /// generation D1 from a later AwaitingDescendants generation D2.
    pub released_owner_deferred_turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentCompletionHandoffDisposition {
    Stored(StoredAgentCompletionHandoff),
    SuppressedByTreeStop,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct OwnerResumeRequestId(HistoryItemId);

impl From<HistoryItemId> for OwnerResumeRequestId {
    fn from(value: HistoryItemId) -> Self {
        Self(value)
    }
}

impl Display for OwnerResumeRequestId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl FromStr for OwnerResumeRequestId {
    type Err = ulid::DecodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse::<HistoryItemId>().map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerResumeRequestState {
    Pending,
    Claimed,
    Resolved,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerResumeRequest {
    pub request_id: OwnerResumeRequestId,
    pub owner_session_id: SessionId,
    pub source_session_id: SessionId,
    pub state: OwnerResumeRequestState,
    pub claimed_turn_id: Option<TurnId>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredAgentCompletionKind {
    CompletedEarly,
    CrashFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredAgentCompletionState {
    Pending,
    Superseded,
    Released,
    Discarded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeferredAgentCompletion {
    pub agent_session_id: SessionId,
    pub agent_turn_id: TurnId,
    pub parent_session_id: SessionId,
    pub kind: DeferredAgentCompletionKind,
    pub state: DeferredAgentCompletionState,
    pub resolved_by_terminal_event_id: Option<RuntimeEventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AgentTerminalEffects {
    pub completion_handoff: Option<StoredAgentCompletionHandoff>,
    pub deferred: Option<DeferredAgentCompletion>,
    pub released_deferred_handoffs: Vec<StoredAgentCompletionHandoff>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingAgentTriggerSettlement {
    Applied {
        turn_id: TurnId,
        handoff: Option<StoredAgentCompletionHandoff>,
    },
    BlockedByPendingDeferredCompletion {
        deferred_turn_id: TurnId,
    },
    WakeOwnedOrResolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentExecutionWakeTerminalOwner {
    ExplicitTask(HistoryItemId),
    OwnerResume(OwnerResumeRequestId),
}

#[derive(Debug, Clone)]
pub(crate) enum AgentExecutionWakeTerminalSettlement {
    Applied {
        turn_id: TurnId,
        terminal: DurableTurnTerminal,
    },
    AlreadyTerminal {
        turn_id: TurnId,
        terminal: DurableTurnTerminal,
    },
    BlockedByPendingDeferredCompletion {
        deferred_turn_id: TurnId,
    },
    WakeUnavailable,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredAgentSpawn {
    pub child_session: SessionRecord,
    pub edge: SessionSpawnEdge,
    pub initial_task_history_item_id: HistoryItemId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalOwnerGuard {
    Admitted {
        admission_id: AdmissionId,
        turn_id: TurnId,
    },
    Captured(RunningSessionTerminalTarget),
    AgentWake(AgentExecutionWakeTerminalOwner),
}

#[derive(Debug, Clone)]
enum GuardedTerminalization {
    Settled {
        commit: AdmittedTerminalCommit,
        turn_id: TurnId,
        terminal: DurableTurnTerminal,
    },
    BlockedByPendingDeferredCompletion {
        deferred_turn_id: TurnId,
    },
    NotOwned,
}

impl GuardedTerminalization {
    fn admitted_commit(self) -> Result<AdmittedTerminalCommit, StorageError> {
        match self {
            Self::Settled { commit, .. } => Ok(commit),
            Self::NotOwned => Ok(AdmittedTerminalCommit::NotOwned),
            Self::BlockedByPendingDeferredCompletion { deferred_turn_id } => {
                Err(StorageError::Message(format!(
                    "terminal settlement remained blocked by deferred turn {deferred_turn_id}"
                )))
            }
        }
    }
}

impl DurableRunAdmission {
    fn is_fresh_at(self, now_ms: i64) -> bool {
        self.lease_expires_at_ms > normalize_run_lease_now_ms(now_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedSessionRuntimeState {
    status: SessionStatus,
    admission: Option<DurableRunAdmission>,
}

impl ValidatedSessionRuntimeState {
    fn fresh_admission_at(self, now_ms: i64) -> Option<DurableRunAdmission> {
        self.admission
            .filter(|admission| admission.is_fresh_at(now_ms))
    }

    fn fresh_running_turn_at(self, now_ms: i64) -> Option<TurnId> {
        (self.status == SessionStatus::Running)
            .then(|| self.fresh_admission_at(now_ms))
            .flatten()
            .map(|admission| admission.turn_id)
    }

    fn blocks_mutation_at(self, now_ms: i64) -> bool {
        self.status == SessionStatus::Running || self.fresh_admission_at(now_ms).is_some()
    }

    fn blocks_tree_mutation(self) -> bool {
        self.admission.is_some()
    }

    fn stop_state(self) -> DurableSessionStopState {
        match self.status {
            SessionStatus::Idle => DurableSessionStopState::Idle,
            SessionStatus::Running => {
                DurableSessionStopState::Running(RunningSessionTerminalTarget::from_admission(
                    self.admission
                        .expect("running session admission validated before stop projection"),
                ))
            }
            SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed => {
                DurableSessionStopState::Terminal(self.status)
            }
        }
    }
}

#[derive(Debug)]
struct RawSessionRuntimeState {
    status: String,
    active_run_id: Option<String>,
    active_turn_id: Option<String>,
    active_run_lease_expires_at_ms: Option<i64>,
    terminal_count: i64,
    terminal_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionProjectionState {
    pub session: SessionRecord,
    pub archived: bool,
    pub active_turn_id: Option<TurnId>,
    pub active_turn_sequence_no: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalSessionStorageSnapshot {
    pub session: SessionRecord,
    pub protocol: CanonicalProtocolSnapshot,
    pub active_turn_position: Option<(TurnId, i64)>,
    pub pending_turn_inputs: Vec<crate::session::PendingTurnInputProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescendantRunAdmissionState {
    pub edge: SessionSpawnEdge,
    pub blocks_new_root_turn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedThreadGoal {
    pub goal_id: String,
    pub goal: ThreadGoal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedTurnSnapshot {
    pub admission_id: AdmissionId,
    pub goal: Option<AdmittedThreadGoal>,
    pub initial_user_history_item_id: Option<HistoryItemId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveGoalTurnAdmission {
    Admitted(AdmittedTurnSnapshot),
    GoalInactive,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TurnGoalAdmissionChange {
    Preserve,
    SetObjective(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnGoalAdmissionRequirement {
    Any,
    Active,
}

#[derive(Debug, Clone)]
struct TurnAdmissionRequest {
    turn_id: TurnId,
    goal_change: TurnGoalAdmissionChange,
    goal_requirement: TurnGoalAdmissionRequirement,
    initial_user_turn: Option<UserTurn>,
    expected_agent_trigger_history_item_id: Option<HistoryItemId>,
    expected_owner_resume_request_id: Option<OwnerResumeRequestId>,
}

impl TurnAdmissionRequest {
    fn preserve_goal(turn_id: TurnId, initial_user_turn: Option<&UserTurn>) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::Preserve,
            goal_requirement: TurnGoalAdmissionRequirement::Any,
            initial_user_turn: initial_user_turn.cloned(),
            expected_agent_trigger_history_item_id: None,
            expected_owner_resume_request_id: None,
        }
    }

    fn for_agent_trigger(turn_id: TurnId, history_item_id: HistoryItemId) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::Preserve,
            goal_requirement: TurnGoalAdmissionRequirement::Any,
            initial_user_turn: None,
            expected_agent_trigger_history_item_id: Some(history_item_id),
            expected_owner_resume_request_id: None,
        }
    }

    fn for_owner_resume(turn_id: TurnId, request_id: OwnerResumeRequestId) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::Preserve,
            goal_requirement: TurnGoalAdmissionRequirement::Any,
            initial_user_turn: None,
            expected_agent_trigger_history_item_id: None,
            expected_owner_resume_request_id: Some(request_id),
        }
    }

    fn require_active_goal(turn_id: TurnId, initial_user_turn: Option<&UserTurn>) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::Preserve,
            goal_requirement: TurnGoalAdmissionRequirement::Active,
            initial_user_turn: initial_user_turn.cloned(),
            expected_agent_trigger_history_item_id: None,
            expected_owner_resume_request_id: None,
        }
    }

    fn set_goal_objective(
        turn_id: TurnId,
        objective: impl Into<String>,
        initial_user_turn: Option<&UserTurn>,
    ) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::SetObjective(objective.into()),
            goal_requirement: TurnGoalAdmissionRequirement::Any,
            initial_user_turn: initial_user_turn.cloned(),
            expected_agent_trigger_history_item_id: None,
            expected_owner_resume_request_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmittedTerminalCommit {
    Applied,
    AlreadyTerminalizedBySameAdmission,
    NotOwned,
}

#[derive(Debug, Clone)]
pub enum RunAdmissionLeaseRenewalOutcome {
    Renewed,
    StopFenced(TurnTerminalOutcome),
    Terminal(crate::session::model::DurableTurnTerminal),
    SupersededOrExpired,
}

#[derive(Debug, Clone)]
pub(crate) enum AdmittedRunState {
    OwnedRunning,
    StopFenced(TurnTerminalOutcome),
    Terminal(DurableTurnTerminal),
    SupersededOrExpired,
}

impl AdmittedTerminalCommit {
    pub fn was_applied(self) -> bool {
        self == Self::Applied
    }

    pub fn ended_owned_run(self) -> bool {
        matches!(
            self,
            Self::Applied | Self::AlreadyTerminalizedBySameAdmission
        )
    }
}

impl SqliteSessionRepository {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    #[cfg(test)]
    pub(crate) async fn insert_session_spawn_edge(
        &self,
        root_session_id: SessionId,
        parent_session_id: SessionId,
        child_session_id: SessionId,
        agent_path: &str,
        task_name: &str,
    ) -> Result<SessionSpawnEdge, StorageError> {
        validate_session_spawn_edge_shape(
            root_session_id,
            parent_session_id,
            child_session_id,
            agent_path,
            task_name,
        )
        .map_err(StorageError::Message)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let edge = insert_session_spawn_edge_in_transaction(
            &transaction,
            root_session_id,
            parent_session_id,
            child_session_id,
            agent_path,
            task_name,
        )?;
        transaction.commit()?;
        Ok(edge)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_agent_spawn_with_initial_task_for_caller_turn(
        &self,
        root_session_id: SessionId,
        caller_session_id: SessionId,
        child_session_id: SessionId,
        child_draft: NewSession,
        agent_path: &str,
        task_name: &str,
        caller_admission_id: AdmissionId,
        caller_turn_id: TurnId,
        context_fork: SpawnContextFork,
        initial_task: InterAgentCommunication,
    ) -> Result<StoredAgentSpawn, StorageError> {
        validate_session_spawn_edge_shape(
            root_session_id,
            caller_session_id,
            child_session_id,
            agent_path,
            task_name,
        )
        .map_err(StorageError::Message)?;
        if !initial_task.trigger_turn {
            return Err(StorageError::Message(
                "an initial agent task must trigger the child turn".to_string(),
            ));
        }
        if initial_task.recipient != agent_path {
            return Err(StorageError::Message(format!(
                "initial agent task targets `{}` instead of durable child path `{agent_path}`",
                initial_task.recipient
            )));
        }
        let child_draft = normalize_new_session_draft(child_draft)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());

        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            caller_session_id,
            caller_admission_id,
            caller_turn_id,
        )?;
        let root_session = session_record_from_connection(&transaction, root_session_id)?;
        let caller_session = session_record_from_connection(&transaction, caller_session_id)?;
        validate_agent_child_session_draft(
            &root_session,
            &caller_session,
            &child_draft,
            agent_path,
            task_name,
            &initial_task,
        )?;
        let child_session =
            insert_session_in_transaction(&transaction, child_session_id, &child_draft, now)?;
        let edge = insert_session_spawn_edge_in_transaction(
            &transaction,
            root_session_id,
            caller_session_id,
            child_session_id,
            agent_path,
            task_name,
        )?;
        match context_fork {
            SpawnContextFork::None => {}
            SpawnContextFork::All => {
                fork_agent_context_in_transaction_for_spawn(
                    &transaction,
                    caller_session_id,
                    child_session_id,
                    None,
                )?;
            }
            SpawnContextFork::Recent(turns) => {
                fork_agent_context_in_transaction_for_spawn(
                    &transaction,
                    caller_session_id,
                    child_session_id,
                    Some(turns),
                )?;
            }
        }
        let initial_task_history_item_id = insert_agent_mailbox_message_in_transaction(
            &transaction,
            root_session_id,
            caller_session_id,
            child_session_id,
            initial_task,
            now,
            true,
        )?;
        transaction.commit()?;
        Ok(StoredAgentSpawn {
            child_session,
            edge,
            initial_task_history_item_id,
        })
    }

    pub async fn session_spawn_edge_for_child(
        &self,
        child_session_id: SessionId,
    ) -> Result<Option<SessionSpawnEdge>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let edge = connection
            .query_row(
                "SELECT root_session_id, parent_session_id, child_session_id,
                        agent_path, task_name, spawn_order, created_at_ms
                 FROM session_spawn_edges
                 WHERE child_session_id = ?1",
                params![child_session_id.to_string()],
                session_spawn_edge_from_row,
            )
            .optional()
            .map_err(StorageError::from)?;
        if let Some(edge) = edge.as_ref() {
            validate_session_spawn_edge_parent(&connection, edge)?;
        }
        Ok(edge)
    }

    pub async fn list_session_spawn_edges(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<SessionSpawnEdge>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT root_session_id, parent_session_id, child_session_id,
                    agent_path, task_name, spawn_order, created_at_ms
             FROM session_spawn_edges
             WHERE root_session_id = ?1
             ORDER BY spawn_order ASC, child_session_id ASC",
        )?;
        let edges = statement
            .query_map(
                params![root_session_id.to_string()],
                session_spawn_edge_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)?;
        validate_session_spawn_edge_tree(&edges)?;
        Ok(edges)
    }

    pub async fn list_session_subtree_ids(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<SessionId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "WITH RECURSIVE tree_root(root_session_id) AS (
                 SELECT COALESCE(
                     (SELECT root_session_id
                      FROM session_spawn_edges
                      WHERE child_session_id = ?1),
                     ?1
                 )
             ),
             subtree(session_id) AS (
                 SELECT session.id
                 FROM sessions AS session
                 WHERE session.id = ?1
                 UNION
                 SELECT edge.child_session_id
                 FROM session_spawn_edges AS edge
                 INNER JOIN subtree
                   ON edge.parent_session_id = subtree.session_id
                 INNER JOIN tree_root
                   ON edge.root_session_id = tree_root.root_session_id
             )
             SELECT subtree.session_id
             FROM subtree
             LEFT JOIN session_spawn_edges AS edge
               ON edge.child_session_id = subtree.session_id
              AND edge.root_session_id = (SELECT root_session_id FROM tree_root)
             ORDER BY
                 CASE WHEN subtree.session_id = ?1 THEN 0 ELSE 1 END,
                 (LENGTH(edge.agent_path) - LENGTH(REPLACE(edge.agent_path, '/', ''))) ASC,
                 edge.spawn_order ASC,
                 subtree.session_id ASC",
        )?;
        statement
            .query_map(params![session_id.to_string()], |row| {
                parse_session_id_column(row, 0)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Reads every retained descendant and its validated durable runtime state in one SQL
    /// snapshot. The caller may combine this semantic projection with its process-local run
    /// registry without issuing one query per child.
    pub async fn list_descendant_run_admission_states(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<DescendantRunAdmissionState>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT edge.root_session_id, edge.parent_session_id, edge.child_session_id,
                     edge.agent_path, edge.task_name, edge.spawn_order, edge.created_at_ms,
                     child.status, child.active_run_id, child.active_turn_id,
                     child.active_run_lease_expires_at_ms,
                     (SELECT COUNT(*)
                      FROM protocol_runtime_events AS terminal_event
                      WHERE terminal_event.session_id = child.id
                        AND terminal_event.turn_id = child.active_turn_id
                        AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                     (SELECT terminal_event.msg_json
                      FROM protocol_runtime_events AS terminal_event
                      WHERE terminal_event.session_id = child.id
                        AND terminal_event.turn_id = child.active_turn_id
                        AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                      ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC
                      LIMIT 1)
              FROM session_spawn_edges AS edge
             INNER JOIN sessions AS child ON child.id = edge.child_session_id
             WHERE edge.root_session_id = ?1
             ORDER BY edge.spawn_order ASC, edge.child_session_id ASC",
        )?;
        let rows = statement
            .query_map(params![root_session_id.to_string()], |row| {
                Ok((
                    session_spawn_edge_from_row(row)?,
                    raw_session_runtime_state_from_row(row, 7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        validate_session_spawn_edge_tree(
            &rows
                .iter()
                .map(|(edge, _)| edge.clone())
                .collect::<Vec<_>>(),
        )?;
        rows.into_iter()
            .map(|(edge, raw)| {
                let runtime_state = validate_raw_session_runtime_state(edge.child_session_id, raw)?;
                let admission_is_tree_stop_fenced =
                    runtime_state_admission_started_before_tree_stop_fence(
                        &connection,
                        edge.child_session_id,
                        runtime_state,
                    )?;
                Ok(DescendantRunAdmissionState {
                    edge,
                    blocks_new_root_turn: runtime_state.blocks_mutation_at(now)
                        && !admission_is_tree_stop_fenced,
                })
            })
            .collect()
    }

    pub async fn compare_and_set_root_session_access_mode(
        &self,
        session_id: SessionId,
        expected_access_mode: AccessMode,
        access_mode: AccessMode,
    ) -> Result<Option<SessionSettingsUpdate>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = session_record_from_connection(&transaction, session_id)?;
        let is_child = transaction
            .query_row(
                "SELECT 1 FROM session_spawn_edges WHERE child_session_id = ?1",
                params![session_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if is_child {
            return Err(StorageError::Message(format!(
                "session {session_id} is a child agent session; root access mode ownership was rejected"
            )));
        }
        if current.access_mode != expected_access_mode {
            transaction.commit()?;
            return Ok(None);
        }
        if current.access_mode == access_mode {
            transaction.commit()?;
            return Ok(Some(SessionSettingsUpdate {
                session: current,
                changed: false,
            }));
        }
        let now = SystemClock::now_ms().max(current.updated_at_ms.saturating_add(1));
        let updated = transaction.execute(
            "UPDATE sessions
             SET access_mode = ?3, updated_at_ms = ?4
             WHERE id = ?1
               AND access_mode = ?2
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )",
            params![
                session_id.to_string(),
                expected_access_mode.as_str(),
                access_mode.as_str(),
                now
            ],
        )?;
        if updated != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let session = session_record_from_connection(&transaction, session_id)?;
        transaction.commit()?;
        Ok(Some(SessionSettingsUpdate {
            session,
            changed: true,
        }))
    }

    pub async fn running_session_recovery_fence(&self) -> Result<Option<SessionId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .query_row(
                "SELECT id
                 FROM sessions
                 WHERE status = 'running'
                 ORDER BY id DESC
                 LIMIT 1",
                [],
                |row| parse_session_id_column(row, 0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub(crate) async fn running_session_recovery_page(
        &self,
        after: Option<RunningSessionRecoveryCursor>,
        through: SessionId,
        limit: usize,
    ) -> Result<Vec<RunningSessionRecoveryCandidate>, StorageError> {
        let limit = sqlite_limit(limit)?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "WITH recovery_candidates AS (
                 SELECT
                     sessions.id, sessions.project_id, sessions.title, sessions.status,
                     sessions.cwd_path, sessions.model_name, sessions.base_url,
                     sessions.access_mode, sessions.model_parameters_json,
                     sessions.created_at_ms, sessions.updated_at_ms,
                     sessions.completed_at_ms, sessions.status,
                     sessions.active_run_id, sessions.active_turn_id,
                     sessions.active_run_lease_expires_at_ms,
                     (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                      WHERE terminal_event.session_id = sessions.id
                        AND terminal_event.turn_id = sessions.active_turn_id
                        AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                     (SELECT terminal_event.msg_json
                      FROM protocol_runtime_events AS terminal_event
                      WHERE terminal_event.session_id = sessions.id
                        AND terminal_event.turn_id = sessions.active_turn_id
                        AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                      ORDER BY terminal_event.sequence_no DESC,
                               terminal_event.rowid DESC
                      LIMIT 1),
                     COALESCE(
                         LENGTH(edge.agent_path)
                           - LENGTH(REPLACE(edge.agent_path, '/', '')),
                         0
                     ) AS recovery_depth
                 FROM sessions
                 LEFT JOIN session_spawn_edges AS edge
                   ON edge.child_session_id = sessions.id
                 WHERE sessions.status = 'running'
                   AND sessions.id <= ?3
             )
             SELECT *
             FROM recovery_candidates
             WHERE ?1 IS NULL
                OR recovery_depth < ?1
                OR (recovery_depth = ?1 AND id > ?2)
             ORDER BY recovery_depth DESC, id ASC
             LIMIT ?4",
        )?;
        let after_depth = after.map(|cursor| cursor.recovery_depth);
        let after_session_id = after.map(|cursor| cursor.session_id.to_string());
        let rows = statement
            .query_map(
                params![after_depth, after_session_id, through.to_string(), limit],
                |row| {
                    Ok((
                        session_record_with_identity_from_row(row)?,
                        raw_session_runtime_state_from_row(row, 12)?,
                        row.get::<_, i64>(18)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(session, raw, recovery_depth)| {
                let runtime_state = validate_raw_session_runtime_state(session.id, raw)?;
                let DurableSessionStopState::Running(terminal_target) = runtime_state.stop_state()
                else {
                    return Err(StorageError::Message(format!(
                        "running-session recovery query returned non-running session {}",
                        session.id
                    )));
                };
                Ok(RunningSessionRecoveryCandidate {
                    session,
                    terminal_target,
                    recovery_depth,
                })
            })
            .collect()
    }

    pub async fn delete_session_tree(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<SessionId>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            params![session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?;
        if !session_exists {
            transaction.commit()?;
            return Ok(Vec::new());
        }
        if let Some(active_session_id) =
            active_session_for_mutation_branch(&transaction, session_id, true)?
        {
            return Err(StorageError::Message(format!(
                "session {session_id} has active or pending agent-tree session {active_session_id}; stop the agent tree before deleting it"
            )));
        }
        let mut statement = transaction.prepare(
            "WITH RECURSIVE tree_root(root_session_id) AS (
                 SELECT COALESCE(
                     (SELECT root_session_id
                      FROM session_spawn_edges
                      WHERE child_session_id = ?1),
                     ?1
                 )
             ),
             subtree(session_id) AS (
                 SELECT ?1
                 UNION
                 SELECT edge.child_session_id
                 FROM session_spawn_edges AS edge
                 INNER JOIN subtree
                   ON edge.parent_session_id = subtree.session_id
                 INNER JOIN tree_root
                   ON edge.root_session_id = tree_root.root_session_id
             )
             SELECT subtree.session_id
             FROM subtree
             LEFT JOIN session_spawn_edges AS edge
               ON edge.child_session_id = subtree.session_id
              AND edge.root_session_id = (SELECT root_session_id FROM tree_root)
             ORDER BY
                 (LENGTH(edge.agent_path) - LENGTH(REPLACE(edge.agent_path, '/', ''))) DESC,
                 edge.created_at_ms DESC,
                 subtree.session_id ASC",
        )?;
        let deleted_session_ids = statement
            .query_map(params![session_id.to_string()], |row| {
                parse_session_id_column(row, 0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        prepare_agent_mailbox_for_session_tree_delete(
            &transaction,
            session_id,
            &deleted_session_ids,
        )?;
        for deleted_session_id in &deleted_session_ids {
            delete_session_rows(&transaction, *deleted_session_id)?;
        }
        transaction.commit()?;
        Ok(deleted_session_ids)
    }

    pub async fn session_is_archived(&self, session_id: SessionId) -> Result<bool, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .query_row(
                "SELECT archived_at_ms IS NOT NULL FROM sessions WHERE id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(StorageError::from)
    }

    pub(crate) async fn canonical_session_protocol_snapshot(
        &self,
        session_id: SessionId,
        history: ProtocolPageRequest,
        turns: ProtocolPageRequest,
    ) -> Result<CanonicalSessionStorageSnapshot, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let session = session_record_from_connection(&transaction, session_id)?;
        let protocol =
            canonical_protocol_snapshot_from_connection(&transaction, session_id, history, turns)?;
        let runtime_state = session_runtime_state_from_connection(&transaction, session_id)?
            .expect("session record loaded in the same transaction");
        let pending_turn_inputs =
            pending_turn_input_projections_in_transaction(&transaction, session_id, runtime_state)?;
        let active_turn_position = if runtime_state.status == SessionStatus::Running {
            let turn_id = runtime_state
                .admission
                .expect("running admission validated before canonical snapshot")
                .turn_id;
            let sequence_no = transaction
                .query_row(
                    "SELECT next_sequence_no
                 FROM protocol_turn_sequence_allocators
                 WHERE session_id = ?1 AND turn_id = ?2",
                    params![session_id.to_string(), turn_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            Some((turn_id, sequence_no))
        } else {
            None
        };
        transaction.commit()?;
        Ok(CanonicalSessionStorageSnapshot {
            session,
            protocol,
            active_turn_position,
            pending_turn_inputs,
        })
    }

    pub async fn session_projection_state(
        &self,
        session_id: SessionId,
    ) -> Result<SessionProjectionState, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    archived_at_ms IS NOT NULL, active_run_id, active_turn_id,
                    active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1),
                    (SELECT allocator.next_sequence_no
                     FROM protocol_turn_sequence_allocators AS allocator
                     WHERE allocator.session_id = sessions.id
                       AND allocator.turn_id = sessions.active_turn_id)
             FROM sessions
             WHERE id = ?1",
        )?;
        let row =
            statement.query_row(params![session_id.to_string()], session_projection_from_row)?;
        validate_session_projection_state(row)
    }

    pub async fn list_sessions_with_projection_state(
        &self,
        project_id: ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionProjectionState>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    archived_at_ms IS NOT NULL, active_run_id, active_turn_id,
                    active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1),
                    (SELECT allocator.next_sequence_no
                     FROM protocol_turn_sequence_allocators AS allocator
                     WHERE allocator.session_id = sessions.id
                       AND allocator.turn_id = sessions.active_turn_id)
             FROM sessions
             WHERE project_id = ?1{archived_filter}
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?2"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(
                params![project_id.to_string(), sqlite_limit(limit)?],
                session_projection_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(validate_session_projection_state)
            .collect()
    }

    pub async fn search_sessions_with_projection_state(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionProjectionState>, StorageError> {
        let normalized = format!(
            "%{}%",
            escape_like_literal(&query.trim().to_ascii_lowercase())
        );
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    archived_at_ms IS NOT NULL, active_run_id, active_turn_id,
                    active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1),
                    (SELECT allocator.next_sequence_no
                     FROM protocol_turn_sequence_allocators AS allocator
                     WHERE allocator.session_id = sessions.id
                       AND allocator.turn_id = sessions.active_turn_id)
             FROM sessions
             WHERE project_id = ?1{archived_filter}
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
               AND (
                   lower(title) LIKE ?2 ESCAPE '\\'
                   OR lower(cwd_path) LIKE ?2 ESCAPE '\\'
                   OR lower(model_name) LIKE ?2 ESCAPE '\\'
                   OR lower(base_url) LIKE ?2 ESCAPE '\\'
                   OR lower(access_mode) LIKE ?2 ESCAPE '\\'
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?3"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(
                params![project_id.to_string(), normalized, sqlite_limit(limit)?],
                session_projection_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(validate_session_projection_state)
            .collect()
    }

    pub async fn session_owns_truncated_output(
        &self,
        session_id: SessionId,
        path: &camino::Utf8Path,
    ) -> Result<bool, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let owned = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM tool_calls AS tool
                 INNER JOIN protocol_history_items AS history
                    ON history.id = tool.history_item_id
                 WHERE history.session_id = ?1
                   AND tool.truncated_output_path = ?2
             )",
            params![session_id.to_string(), path.as_str()],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(owned)
    }

    pub async fn rollback_session_transaction(
        &self,
        session_id: SessionId,
        num_turns: usize,
    ) -> Result<crate::session::SessionRollbackResult, StorageError> {
        if num_turns == 0 {
            return Err(StorageError::Message(
                "session rollback turn count must be greater than zero".to_string(),
            ));
        }
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        session_record_from_connection(&transaction, session_id)?;
        let root_session_id = transaction
            .query_row(
                "SELECT root_session_id
                 FROM session_spawn_edges
                 WHERE child_session_id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| {
                value
                    .parse::<SessionId>()
                    .map_err(|error| StorageError::Message(error.to_string()))
            })
            .transpose()?
            .unwrap_or(session_id);
        let active_tree_session =
            active_session_for_mutation_branch(&transaction, root_session_id, true)?;
        if let Some(active_tree_session) = active_tree_session {
            return Err(StorageError::Message(format!(
                "session {session_id} belongs to agent tree {root_session_id}, which still has active or pending session {active_tree_session}; stop the complete agent tree before rollback"
            )));
        }

        let dropped_turn_ids =
            latest_protocol_turn_ids_in_transaction(&transaction, session_id, num_turns)?;
        if dropped_turn_ids.len() < num_turns {
            return Err(StorageError::Message(format!(
                "cannot rollback {num_turns} turn(s); session {session_id} only has {} canonical turn(s)",
                dropped_turn_ids.len()
            )));
        }
        for turn_id in &dropped_turn_ids {
            let delivered_mailbox_count = transaction.query_row(
                "SELECT COUNT(*)
                 FROM agent_mailbox_messages
                 WHERE recipient_session_id = ?1
                   AND state = 'delivered'
                   AND delivered_turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )?;
            if delivered_mailbox_count != 0 {
                return Err(StorageError::Message(format!(
                    "cannot rollback session {session_id} turn {turn_id} because it contains {delivered_mailbox_count} delivered agent mailbox message(s); mailbox delivery is immutable"
                )));
            }
            let completion_handoff_count = transaction.query_row(
                "SELECT COUNT(*)
                 FROM agent_completion_handoffs
                 WHERE (child_session_id = ?1 AND child_turn_id = ?2)
                    OR parent_history_item_id IN (
                        SELECT id
                        FROM protocol_history_items
                        WHERE session_id = ?1
                          AND scope_kind = 'turn'
                          AND turn_id = ?2
                    )",
                params![session_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )?;
            if completion_handoff_count != 0 {
                return Err(StorageError::Message(format!(
                    "cannot rollback session {session_id} turn {turn_id} because it participates in {completion_handoff_count} durable agent completion handoff(s)"
                )));
            }
            let deferred_completion_count = transaction.query_row(
                "SELECT COUNT(*)
                 FROM agent_deferred_completions
                 WHERE agent_session_id = ?1 AND agent_turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )?;
            if deferred_completion_count != 0 {
                return Err(StorageError::Message(format!(
                    "cannot rollback session {session_id} turn {turn_id} because it participates in {deferred_completion_count} durable deferred completion receipt(s)"
                )));
            }
            let owner_resume_claim_count = transaction.query_row(
                "SELECT COUNT(*)
                 FROM agent_owner_resume_requests
                 WHERE owner_session_id = ?1
                   AND claimed_turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )?;
            if owner_resume_claim_count != 0 {
                return Err(StorageError::Message(format!(
                    "cannot rollback session {session_id} turn {turn_id} because it owns {owner_resume_claim_count} durable OwnerResume request claim(s)"
                )));
            }
            let explicit_wake_claim_count = transaction.query_row(
                "SELECT COUNT(*)
                 FROM agent_trigger_turn_claims
                 WHERE recipient_session_id = ?1
                   AND turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )?;
            if explicit_wake_claim_count != 0 {
                return Err(StorageError::Message(format!(
                    "cannot rollback session {session_id} turn {turn_id} because it owns {explicit_wake_claim_count} durable explicit agent wake claim(s)"
                )));
            }
            transaction.execute(
                "DELETE FROM turn_steer_inputs
                 WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
            )?;
            transaction.execute(
                "DELETE FROM protocol_turn_items WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
            )?;
            transaction.execute(
                "DELETE FROM protocol_history_items WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
            )?;
            transaction.execute(
                "DELETE FROM protocol_runtime_events WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
            )?;
            transaction.execute(
                "DELETE FROM protocol_item_append_order WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
            )?;
            transaction.execute(
                "DELETE FROM protocol_turn_sequence_allocators WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id.to_string(), turn_id.to_string()],
            )?;
        }
        transaction.execute(
            "UPDATE sessions
             SET status = 'idle', updated_at_ms = ?2, completed_at_ms = NULL,
                 active_run_id = NULL, active_turn_id = NULL,
                 active_run_lease_expires_at_ms = NULL
             WHERE id = ?1",
            params![session_id.to_string(), now],
        )?;
        let remaining_history_items = transaction.query_row(
            "SELECT COUNT(*) FROM protocol_history_items WHERE session_id = ?1",
            params![session_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let session = session_record_from_connection(&transaction, session_id)?;
        transaction.commit()?;
        Ok(crate::session::SessionRollbackResult {
            session,
            dropped_turn_ids,
            remaining_history_items,
        })
    }

    pub async fn fork_session_snapshot(
        &self,
        source_session_id: SessionId,
        title: Option<String>,
    ) -> Result<SessionForkResult, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = session_record_from_connection(&transaction, source_session_id)?;
        let source_was_active = source.status == SessionStatus::Running;
        let source_runtime_state =
            session_runtime_state_from_connection(&transaction, source_session_id)?
                .expect("source session loaded in the same transaction");
        let title = title
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|| format!("Fork of {}", source.title));
        let target_session_id = SessionId::new();
        let now = SystemClock::now_ms();
        let inserted = transaction.execute(
            "INSERT INTO sessions (
                 id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                 model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms
             )
             SELECT ?2, project_id, ?3, 'idle', cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, ?4, ?4, NULL
             FROM sessions WHERE id = ?1",
            params![
                source_session_id.to_string(),
                target_session_id.to_string(),
                title,
                now
            ],
        )?;
        if inserted != 1 {
            return Err(StorageError::Message(format!(
                "source session {source_session_id} disappeared while creating its fork"
            )));
        }

        let (copied_history_items, copied_turn_items) = fork_canonical_items_in_transaction(
            &transaction,
            source_session_id,
            target_session_id,
        )?;
        if source_was_active {
            let snapshot_turn_id = source_runtime_state
                .admission
                .expect("running source admission validated before snapshot creation")
                .turn_id;
            append_interrupted_live_snapshot_marker_in_transaction(
                &transaction,
                target_session_id,
                snapshot_turn_id,
                "forked from active live session snapshot",
            )?;
        }
        let forked_session = session_record_from_connection(&transaction, target_session_id)?;
        transaction.commit()?;
        Ok(SessionForkResult {
            source_session: source,
            forked_session,
            copied_history_items,
            copied_turn_items,
            interrupted_live_snapshot: source_was_active,
        })
    }

    pub async fn get_thread_goal(
        &self,
        thread_id: SessionId,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        Ok(self
            .get_stored_thread_goal(thread_id)?
            .map(|stored| stored.goal))
    }

    pub async fn get_thread_goal_with_id(
        &self,
        thread_id: SessionId,
    ) -> Result<Option<(ThreadGoal, String)>, StorageError> {
        Ok(self
            .get_stored_thread_goal(thread_id)?
            .map(|stored| (stored.goal, stored.goal_id)))
    }

    pub async fn replace_thread_goal(
        &self,
        thread_id: SessionId,
        objective: &str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> Result<ThreadGoal, StorageError> {
        validate_goal_objective_and_budget(objective, token_budget)?;
        let goal_id = ulid::Ulid::new().to_string();
        let now = SystemClock.now_ms();
        let status = status_after_budget_limit(status, 0, token_budget);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO thread_goals (
                 thread_id, goal_id, objective, status, token_budget, tokens_used,
                 time_used_seconds, created_at_ms, updated_at_ms
             )
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?7)
             ON CONFLICT(thread_id) DO UPDATE SET
                 goal_id = excluded.goal_id,
                 objective = excluded.objective,
                 status = excluded.status,
                 token_budget = excluded.token_budget,
                 tokens_used = 0,
                 time_used_seconds = 0,
                 created_at_ms = excluded.created_at_ms,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                thread_id.to_string(),
                goal_id,
                objective,
                status.as_db_str(),
                token_budget,
                now,
                now
            ],
        )?;
        drop(connection);
        self.get_thread_goal(thread_id)
            .await?
            .ok_or_else(|| StorageError::Message("thread goal was not stored".to_string()))
    }

    pub async fn insert_thread_goal(
        &self,
        thread_id: SessionId,
        objective: &str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        validate_goal_objective_and_budget(objective, token_budget)?;
        let goal_id = ulid::Ulid::new().to_string();
        let now = SystemClock.now_ms();
        let status = status_after_budget_limit(status, 0, token_budget);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let changed = connection.execute(
            "INSERT INTO thread_goals (
                 thread_id, goal_id, objective, status, token_budget, tokens_used,
                 time_used_seconds, created_at_ms, updated_at_ms
             )
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?7)
             ON CONFLICT(thread_id) DO UPDATE SET
                 goal_id = excluded.goal_id,
                 objective = excluded.objective,
                 status = excluded.status,
                 token_budget = excluded.token_budget,
                 tokens_used = 0,
                 time_used_seconds = 0,
                 created_at_ms = excluded.created_at_ms,
                 updated_at_ms = excluded.updated_at_ms
             WHERE thread_goals.status = 'complete'",
            params![
                thread_id.to_string(),
                goal_id,
                objective,
                status.as_db_str(),
                token_budget,
                now,
                now
            ],
        )?;
        drop(connection);
        if changed == 0 {
            return Ok(None);
        }
        self.get_thread_goal(thread_id).await
    }

    pub async fn update_thread_goal(
        &self,
        thread_id: SessionId,
        objective: Option<&str>,
        status: Option<ThreadGoalStatus>,
        token_budget: Option<Option<i64>>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        self.update_thread_goal_for_goal(thread_id, objective, status, token_budget, None)
            .await
    }

    pub async fn update_thread_goal_for_goal(
        &self,
        thread_id: SessionId,
        objective: Option<&str>,
        status: Option<ThreadGoalStatus>,
        token_budget: Option<Option<i64>>,
        expected_goal_id: Option<&str>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        for _ in 0..8 {
            let Some(stored) = self.get_stored_thread_goal(thread_id)? else {
                return Ok(None);
            };
            if expected_goal_id.is_some_and(|expected| expected != stored.goal_id) {
                return Ok(Some(stored.goal));
            }
            let next_objective = objective
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(stored.goal.objective.as_str())
                .to_string();
            let next_token_budget = token_budget.unwrap_or(stored.goal.token_budget);
            validate_goal_objective_and_budget(&next_objective, next_token_budget)?;
            let requested_status = status.unwrap_or(stored.goal.status);
            let next_status = if stored.goal.status == ThreadGoalStatus::BudgetLimited
                && matches!(
                    requested_status,
                    ThreadGoalStatus::Paused | ThreadGoalStatus::Blocked
                ) {
                ThreadGoalStatus::BudgetLimited
            } else {
                status_after_budget_limit(
                    requested_status,
                    stored.goal.tokens_used,
                    next_token_budget,
                )
            };
            let now = SystemClock::now_ms().max(stored.updated_at_ms.saturating_add(1));
            let connection = self.connection.lock().expect("sqlite mutex poisoned");
            let changed = connection.execute(
                "UPDATE thread_goals
                 SET objective = ?2,
                     status = ?3,
                     token_budget = ?4,
                     updated_at_ms = ?5
                 WHERE thread_id = ?1
                   AND goal_id = ?6
                   AND updated_at_ms = ?7",
                params![
                    thread_id.to_string(),
                    next_objective,
                    next_status.as_db_str(),
                    next_token_budget,
                    now,
                    stored.goal_id,
                    stored.updated_at_ms
                ],
            )?;
            drop(connection);
            if changed == 1 {
                return self.get_thread_goal(thread_id).await;
            }
        }
        Err(StorageError::Message(
            "thread goal changed repeatedly while applying an update; retry the operation"
                .to_string(),
        ))
    }

    pub async fn delete_thread_goal(&self, thread_id: SessionId) -> Result<bool, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let changed = connection.execute(
            "DELETE FROM thread_goals WHERE thread_id = ?1",
            params![thread_id.to_string()],
        )?;
        Ok(changed > 0)
    }

    pub async fn account_thread_goal_usage(
        &self,
        thread_id: SessionId,
        token_delta: i64,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        self.account_thread_goal_usage_for_goal(thread_id, token_delta, None)
            .await
    }

    pub async fn account_thread_goal_usage_for_goal(
        &self,
        thread_id: SessionId,
        token_delta: i64,
        expected_goal_id: Option<&str>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        let token_delta = token_delta.max(0);
        for _ in 0..8 {
            let Some(stored) = self.get_stored_thread_goal(thread_id)? else {
                return Ok(None);
            };
            if expected_goal_id.is_some_and(|expected| expected != stored.goal_id) {
                return Ok(Some(stored.goal));
            }
            if !matches!(
                stored.goal.status,
                ThreadGoalStatus::Active | ThreadGoalStatus::BudgetLimited
            ) {
                return Ok(Some(stored.goal));
            }
            let wall_clock_now = SystemClock.now_ms();
            let time_delta_seconds = ((wall_clock_now - stored.updated_at_ms).max(0)) / 1000;
            if time_delta_seconds == 0 && token_delta == 0 {
                return Ok(Some(stored.goal));
            }
            let tokens_used = stored.goal.tokens_used.saturating_add(token_delta);
            let time_used_seconds = stored
                .goal
                .time_used_seconds
                .saturating_add(time_delta_seconds);
            let status = status_after_budget_limit(
                stored.goal.status,
                tokens_used,
                stored.goal.token_budget,
            );
            let now = wall_clock_now.max(stored.updated_at_ms.saturating_add(1));
            let connection = self.connection.lock().expect("sqlite mutex poisoned");
            let changed = connection.execute(
                "UPDATE thread_goals
                 SET status = ?2,
                     tokens_used = ?3,
                     time_used_seconds = ?4,
                     updated_at_ms = ?5
                 WHERE thread_id = ?1
                   AND goal_id = ?6
                   AND updated_at_ms = ?7",
                params![
                    thread_id.to_string(),
                    status.as_db_str(),
                    tokens_used,
                    time_used_seconds,
                    now,
                    stored.goal_id,
                    stored.updated_at_ms
                ],
            )?;
            drop(connection);
            if changed == 1 {
                return self.get_thread_goal(thread_id).await;
            }
        }
        Err(StorageError::Message(
            "thread goal changed repeatedly while accounting usage; retry the operation"
                .to_string(),
        ))
    }

    fn get_stored_thread_goal(
        &self,
        thread_id: SessionId,
    ) -> Result<Option<StoredThreadGoal>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        stored_thread_goal_from_connection(&connection, thread_id)
    }

    #[cfg(test)]
    pub(crate) async fn append_user_turn_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn: &UserTurn,
        protocol_turn_id: TurnId,
        protocol_sequence_no: i64,
    ) -> Result<(), StorageError> {
        if turn.turn_id != protocol_turn_id {
            return Err(StorageError::Message(format!(
                "user turn identity mismatch: payload turn {} writer turn {protocol_turn_id}",
                turn.turn_id
            )));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        let event = RunEvent::UserTurnStored {
            session_id,
            turn: Box::new(turn.clone()),
        };
        let projection = project_protocol_run_event(
            &event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )
        .ok_or_else(|| {
            StorageError::Message("UserTurnStored did not produce a protocol bundle".to_string())
        })?;
        let stored = insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            &transaction,
            &projection.runtime_event,
            projection.history_item.as_ref(),
            projection.turn_item.as_ref(),
        )?;
        let _history_item = stored.history_item.ok_or_else(|| {
            StorageError::Message(
                "UserTurnStored protocol bundle omitted its canonical history item".to_string(),
            )
        })?;
        transaction.commit()?;
        Ok(())
    }

    pub async fn commit_admitted_compaction_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<(), StorageError> {
        let RunEvent::CompactionCompleted {
            summarized_messages,
            summary,
            replacement_item_ids,
            ..
        } = event
        else {
            return Err(StorageError::Message(
                "compaction writer requires a CompactionCompleted event".to_string(),
            ));
        };
        if replacement_item_ids.is_empty() {
            return Err(StorageError::Message(
                "compaction must replace at least one canonical history item".to_string(),
            ));
        }
        if *summarized_messages != replacement_item_ids.len() {
            return Err(StorageError::Message(format!(
                "compaction count mismatch: summarized {summarized_messages} messages but supplied {} replacement ids",
                replacement_item_ids.len()
            )));
        }
        if summary.trim().is_empty() {
            return Err(StorageError::Message(
                "compaction summary must not be empty".to_string(),
            ));
        }
        let unique_replacements = replacement_item_ids.iter().copied().collect::<HashSet<_>>();
        if unique_replacements.len() != replacement_item_ids.len() {
            return Err(StorageError::Message(
                "compaction replacement ids must be unique".to_string(),
            ));
        }

        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        {
            let mut statement = transaction.prepare(
                "SELECT 1
                 FROM protocol_history_items
                 WHERE id = ?1 AND session_id = ?2",
            )?;
            for replacement_item_id in replacement_item_ids {
                let exists = statement
                    .query_row(
                        params![replacement_item_id.to_string(), session_id.to_string()],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !exists {
                    return Err(StorageError::Message(format!(
                        "compaction replacement item {replacement_item_id} does not belong to session {session_id}"
                    )));
                }
            }
        }
        let sequence_no = match protocol_sequence_no {
            Some(sequence_no) => sequence_no,
            None => resolve_terminal_protocol_sequence_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                None,
            )?,
        };
        let projection =
            project_protocol_run_event(event, Some(session_id), protocol_turn_id, sequence_no)
                .ok_or_else(|| {
                    StorageError::Message(
                        "CompactionCompleted did not produce a protocol bundle".to_string(),
                    )
                })?;
        let stored = insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            &transaction,
            &projection.runtime_event,
            projection.history_item.as_ref(),
            projection.turn_item.as_ref(),
        )?;
        if stored.history_item.is_none() {
            return Err(StorageError::Message(
                "CompactionCompleted protocol bundle omitted canonical history".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn append_inter_agent_communication_with_protocol_bundle(
        &self,
        session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
    ) -> Result<StoredAgentCommunication, StorageError> {
        self.append_inter_agent_communication_with_protocol_bundle_and_capacity(
            session_id,
            communication,
            require_active_recipient,
            true,
        )
    }

    #[cfg(test)]
    pub(crate) fn append_inter_agent_communication_with_protocol_bundle_and_capacity(
        &self,
        session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
        ready_turn_capacity_granted: bool,
    ) -> Result<StoredAgentCommunication, StorageError> {
        self.append_inter_agent_communication_with_optional_caller_owner(
            None,
            session_id,
            communication,
            require_active_recipient,
            ready_turn_capacity_granted,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_inter_agent_communication_for_caller_turn_with_protocol_bundle_and_capacity(
        &self,
        caller_session_id: SessionId,
        caller_admission_id: AdmissionId,
        caller_turn_id: TurnId,
        recipient_session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
        ready_turn_capacity_granted: bool,
    ) -> Result<StoredAgentCommunication, StorageError> {
        self.append_inter_agent_communication_with_optional_caller_owner(
            Some((caller_session_id, caller_admission_id, caller_turn_id)),
            recipient_session_id,
            communication,
            require_active_recipient,
            ready_turn_capacity_granted,
        )
    }

    fn append_inter_agent_communication_with_optional_caller_owner(
        &self,
        caller_owner: Option<(SessionId, AdmissionId, TurnId)>,
        session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
        ready_turn_capacity_granted: bool,
    ) -> Result<StoredAgentCommunication, StorageError> {
        let requested_schedule_turn = communication.trigger_turn;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((caller_session_id, caller_admission_id, caller_turn_id)) = caller_owner {
            require_active_admission_in_transaction(
                &transaction,
                caller_session_id,
                caller_admission_id,
                caller_turn_id,
            )?;
        }
        let (root_session_id, recipient_path) =
            canonical_agent_identity_in_connection(&transaction, session_id)?;
        if communication.recipient != recipient_path.as_str() {
            return Err(StorageError::Message(format!(
                "inter-agent communication targets `{}` instead of canonical recipient `{recipient_path}`",
                communication.recipient
            )));
        }
        let author_session_id = if let Some((caller_session_id, _, _)) = caller_owner {
            let (caller_root_session_id, caller_path) =
                canonical_agent_identity_in_connection(&transaction, caller_session_id)?;
            if caller_root_session_id != root_session_id {
                return Err(StorageError::Message(format!(
                    "inter-agent communication author session {caller_session_id} and recipient session {session_id} belong to different agent trees"
                )));
            }
            if communication.author != caller_path.as_str() {
                return Err(StorageError::Message(format!(
                    "inter-agent communication author `{}` does not match canonical caller `{caller_path}`",
                    communication.author
                )));
            }
            caller_session_id
        } else if communication.author == AgentPath::root().as_str() {
            root_session_id
        } else {
            let author_session_id = transaction
                .query_row(
                    "SELECT child_session_id
                     FROM session_spawn_edges
                     WHERE root_session_id = ?1 AND agent_path = ?2",
                    params![root_session_id.to_string(), communication.author],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StorageError::Message(format!(
                        "inter-agent communication author `{}` is not retained in root tree {root_session_id}",
                        communication.author
                    ))
                })?;
            parse_session_id_text(&author_session_id, "agent mailbox author")?
        };
        let Some(runtime_state) = session_runtime_state_from_connection(&transaction, session_id)?
        else {
            return Err(StorageError::Message(format!(
                "inter-agent communication target session {session_id} does not exist"
            )));
        };
        let active_turn_id = if runtime_state.status == SessionStatus::Running {
            let admission = runtime_state
                .admission
                .expect("running recipient admission validated before mail append");
            if !admission.is_fresh_at(now) {
                return Err(StorageError::Message(format!(
                    "run admission lease expired for recipient session {session_id}"
                )));
            }
            (!turn_started_before_applicable_tree_stop_fence_in_transaction(
                &transaction,
                session_id,
                admission.turn_id,
            )?)
            .then_some(admission.turn_id)
        } else {
            None
        };
        let has_active_admission = active_turn_id.is_some();
        if require_active_recipient && !has_active_admission {
            return Err(StorageError::Message(format!(
                "recipient session {session_id} became terminal before inter-agent communication could be committed"
            )));
        }
        let pending_deferred = if has_active_admission {
            None
        } else {
            deferred_agent_completion_in_connection(
                &transaction,
                session_id,
                None,
                Some("pending"),
            )?
        };
        let schedule_turn = requested_schedule_turn
            && match pending_deferred.as_ref().map(|deferred| deferred.kind) {
                None => !has_active_admission,
                Some(DeferredAgentCompletionKind::CompletedEarly) => false,
                Some(DeferredAgentCompletionKind::CrashFailed) => true,
            };
        if schedule_turn && !ready_turn_capacity_granted {
            return Err(StorageError::AgentCapacityUnavailable { session_id });
        }
        let history_item_id = insert_agent_mailbox_message_in_transaction(
            &transaction,
            root_session_id,
            author_session_id,
            session_id,
            communication,
            now,
            true,
        )?;
        transaction.commit()?;
        Ok(StoredAgentCommunication {
            history_item_id,
            schedule_turn,
        })
    }

    #[cfg(test)]
    pub(crate) fn deliver_pending_agent_mail_for_admitted_turn(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        limit: usize,
    ) -> Result<DeliveredAgentMailboxPage, StorageError> {
        self.deliver_pending_agent_mail_for_admitted_turn_with_selector(
            session_id,
            admission_id,
            turn_id,
            AgentMailboxDeliverySelector::AllPending,
            limit,
        )
    }

    pub(crate) fn deliver_pending_agent_mail_for_admitted_turn_with_selector(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        selector: AgentMailboxDeliverySelector,
        limit: usize,
    ) -> Result<DeliveredAgentMailboxPage, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(&transaction, session_id, admission_id, turn_id)?;
        let delivered = deliver_pending_agent_mail_in_transaction(
            &transaction,
            session_id,
            turn_id,
            selector,
            limit,
            now,
        )?;
        transaction.commit()?;
        Ok(delivered)
    }

    pub(crate) fn agent_mailbox_communications_by_id(
        &self,
        recipient_session_id: SessionId,
        message_ids: &[HistoryItemId],
    ) -> Result<Vec<(HistoryItemId, InterAgentCommunication)>, StorageError> {
        if message_ids.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut communications = HashMap::with_capacity(message_ids.len());
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM agent_mailbox_messages
             WHERE recipient_session_id = ?1 AND id = ?2",
        )?;
        for message_id in message_ids {
            let payload_json = statement
                .query_row(
                    params![
                        recipient_session_id.to_string(),
                        message_id.to_string(),
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StorageError::Message(format!(
                        "mailbox message {message_id} does not belong to recipient session {recipient_session_id}"
                    ))
                })?;
            let payload = serde_json::from_str::<HistoryItemPayload>(&payload_json)?;
            let HistoryItemPayload::InterAgentCommunication { communication } = payload else {
                return Err(StorageError::Message(format!(
                    "mailbox message {message_id} is not inter-agent communication"
                )));
            };
            communications.insert(*message_id, communication);
        }
        Ok(message_ids
            .iter()
            .filter_map(|message_id| {
                communications
                    .remove(message_id)
                    .map(|communication| (*message_id, communication))
            })
            .collect())
    }

    pub(crate) fn has_pending_agent_mailbox_messages(
        &self,
        recipient_session_id: SessionId,
    ) -> Result<bool, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        Ok(count_pending_agent_mailbox_messages(&connection, recipient_session_id)? > 0)
    }

    pub(crate) fn agent_completion_handoff(
        &self,
        child_session_id: SessionId,
        child_turn_id: TurnId,
    ) -> Result<Option<StoredAgentCompletionHandoff>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let row = connection
            .query_row(
                "SELECT
                     handoff.parent_session_id,
                     CASE
                         WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                         ELSE parent_edge.agent_path
                     END,
                     mailbox.id,
                     (
                         SELECT deferred.agent_turn_id
                         FROM effective_agent_deferred_completions AS deferred
                         WHERE deferred.agent_session_id = handoff.parent_session_id
                           AND deferred.state = 'superseded'
                           AND deferred.resolved_by_terminal_event_id =
                               handoff.child_terminal_event_id
                         LIMIT 1
                     )
                 FROM agent_completion_handoffs AS handoff
                 INNER JOIN session_spawn_edges AS edge
                   ON edge.child_session_id = handoff.child_session_id
                  AND edge.parent_session_id = handoff.parent_session_id
                 LEFT JOIN session_spawn_edges AS parent_edge
                   ON parent_edge.root_session_id = edge.root_session_id
                  AND parent_edge.child_session_id = edge.parent_session_id
                 INNER JOIN agent_mailbox_messages AS mailbox
                   ON mailbox.id = handoff.parent_history_item_id
                  AND mailbox.recipient_session_id = handoff.parent_session_id
                 WHERE handoff.child_session_id = ?1
                   AND handoff.child_turn_id = ?2",
                params![child_session_id.to_string(), child_turn_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            parent_session_id,
            parent_agent_path,
            history_item_id,
            released_owner_deferred_turn_id,
        )) = row
        else {
            return Ok(None);
        };
        let parent_session_id =
            parse_session_id_text(&parent_session_id, "agent completion handoff parent")?;
        let parent_agent_path = parent_agent_path.parse::<AgentPath>().map_err(|error| {
            StorageError::Message(format!(
                "agent completion handoff for child {child_session_id} has invalid parent path `{parent_agent_path}`: {error}"
            ))
        })?;
        let history_item_id = history_item_id.parse::<HistoryItemId>().map_err(|error| {
            StorageError::Message(format!(
                "agent completion handoff for child {child_session_id} has invalid history id `{history_item_id}`: {error}"
            ))
        })?;
        let released_owner_deferred_turn_id = released_owner_deferred_turn_id
            .map(|value| {
                value.parse::<TurnId>().map_err(|error| {
                    StorageError::Message(format!(
                        "agent completion handoff for child {child_session_id} has invalid released owner turn id `{value}`: {error}"
                    ))
                })
            })
            .transpose()?;
        Ok(Some(StoredAgentCompletionHandoff {
            child_session_id,
            child_turn_id,
            parent_session_id,
            parent_agent_path,
            history_item_id,
            released_owner_deferred_turn_id,
        }))
    }

    #[cfg(test)]
    pub(crate) fn pending_deferred_completion(
        &self,
        agent_session_id: SessionId,
    ) -> Result<Option<DeferredAgentCompletion>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        deferred_agent_completion_in_connection(
            &connection,
            agent_session_id,
            None,
            Some("pending"),
        )
    }

    pub(crate) fn agent_terminal_effects(
        &self,
        agent_session_id: SessionId,
        agent_turn_id: TurnId,
    ) -> Result<AgentTerminalEffects, StorageError> {
        let completion_handoff = self.agent_completion_handoff(agent_session_id, agent_turn_id)?;
        let (deferred, released_ids) = {
            let connection = self.connection.lock().expect("sqlite mutex poisoned");
            let deferred = deferred_agent_completion_in_connection(
                &connection,
                agent_session_id,
                Some(agent_turn_id),
                None,
            )?;
            let mut statement = connection.prepare(
                "SELECT released.agent_session_id, released.agent_turn_id
                 FROM protocol_runtime_events AS resolver
                 INNER JOIN agent_deferred_completions AS released
                   ON released.resolved_by_terminal_event_id = resolver.id
                  AND released.state = 'released'
                 WHERE resolver.session_id = ?1 AND resolver.turn_id = ?2
                 ORDER BY released.created_at_ms ASC, released.agent_session_id ASC",
            )?;
            let rows = statement
                .query_map(
                    params![agent_session_id.to_string(), agent_turn_id.to_string()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let released_ids = rows
                .into_iter()
                .map(|(session_id, turn_id)| {
                    Ok((
                        parse_session_id_text(
                            &session_id,
                            "released deferred-completion session",
                        )?,
                        turn_id.parse::<TurnId>().map_err(|error| {
                            StorageError::Message(format!(
                                "released deferred completion has invalid turn id `{turn_id}`: {error}"
                            ))
                        })?,
                    ))
                })
                .collect::<Result<Vec<_>, StorageError>>()?;
            (deferred, released_ids)
        };
        let mut released_deferred_handoffs = Vec::with_capacity(released_ids.len());
        for (session_id, turn_id) in released_ids {
            let handoff = self
                .agent_completion_handoff(session_id, turn_id)?
                .ok_or_else(|| {
                    StorageError::Message(format!(
                        "released deferred completion {session_id} turn {turn_id} has no canonical handoff"
                    ))
                })?;
            released_deferred_handoffs.push(handoff);
        }
        Ok(AgentTerminalEffects {
            completion_handoff,
            deferred,
            released_deferred_handoffs,
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_raw_runtime_state_for_corruption_test(
        &self,
        session_id: SessionId,
        status: &str,
        active_run_id: Option<&str>,
        active_turn_id: Option<&str>,
        lease_expires_at_ms: Option<i64>,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE sessions
             SET status = ?2,
                 active_run_id = ?3,
                 active_turn_id = ?4,
                 active_run_lease_expires_at_ms = ?5
             WHERE id = ?1",
            params![
                session_id.to_string(),
                status,
                active_run_id,
                active_turn_id,
                lease_expires_at_ms
            ],
        )?;
        Ok(())
    }

    pub async fn admit_session_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        self.admit_session_turn_with_initial_user_turn(session_id, turn_id, None)
            .await
    }

    pub async fn admit_session_turn_with_initial_user_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        initial_user_turn: Option<&UserTurn>,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::preserve_goal(turn_id, initial_user_turn),
                SystemClock::now_ms(),
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await?
        {
            ActiveGoalTurnAdmission::Admitted(snapshot) => Ok(Some(snapshot)),
            ActiveGoalTurnAdmission::Unavailable => Ok(None),
            ActiveGoalTurnAdmission::GoalInactive => {
                unreachable!("unconditional admission cannot reject an inactive goal")
            }
        }
    }

    pub(crate) async fn admit_agent_triggered_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        expected_history_item_id: HistoryItemId,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::for_agent_trigger(turn_id, expected_history_item_id),
                SystemClock::now_ms(),
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await?
        {
            ActiveGoalTurnAdmission::Admitted(snapshot) => Ok(Some(snapshot)),
            ActiveGoalTurnAdmission::Unavailable => Ok(None),
            ActiveGoalTurnAdmission::GoalInactive => {
                unreachable!("agent-triggered admission does not require an active goal")
            }
        }
    }

    pub(crate) async fn admit_owner_resume_turn(
        &self,
        owner_session_id: SessionId,
        turn_id: TurnId,
        expected_request_id: OwnerResumeRequestId,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        match self
            .admit_session_turn_request_at(
                owner_session_id,
                TurnAdmissionRequest::for_owner_resume(turn_id, expected_request_id),
                SystemClock::now_ms(),
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await?
        {
            ActiveGoalTurnAdmission::Admitted(snapshot) => Ok(Some(snapshot)),
            ActiveGoalTurnAdmission::Unavailable => Ok(None),
            ActiveGoalTurnAdmission::GoalInactive => {
                unreachable!("owner-resume admission does not require an active goal")
            }
        }
    }

    pub(crate) fn settle_pending_agent_trigger_with_terminal(
        &self,
        session_id: SessionId,
        expected_history_item_id: HistoryItemId,
        terminal: DurableTurnTerminal,
    ) -> Result<PendingAgentTriggerSettlement, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let settlement = settle_pending_agent_trigger_in_transaction(
            &transaction,
            session_id,
            expected_history_item_id,
            now,
            terminal,
            None,
        )?;
        transaction.commit()?;
        Ok(settlement)
    }

    pub(crate) fn settle_pending_agent_trigger_at_tree_stop_fence(
        &self,
        session_id: SessionId,
        expected_history_item_id: HistoryItemId,
        fence: AgentTreeStopFence,
    ) -> Result<PendingAgentTriggerSettlement, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(trigger_append_position) =
            agent_trigger_append_position_authorized_by_tree_stop_fence_in_transaction(
                &transaction,
                session_id,
                expected_history_item_id,
                fence,
            )?
        else {
            transaction.commit()?;
            return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
        };
        let Some(first_fence) = first_applicable_tree_stop_fence_at_append_position_in_connection(
            &transaction,
            session_id,
            trigger_append_position,
        )?
        else {
            transaction.commit()?;
            return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
        };
        if first_fence.root_session_id != fence.root_session_id
            || first_fence.after_append_position > fence.after_append_position
        {
            transaction.commit()?;
            return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
        }
        let settlement = settle_pending_agent_trigger_in_transaction(
            &transaction,
            session_id,
            expected_history_item_id,
            now,
            DurableTurnTerminal {
                outcome: recovery_terminal_outcome_for_tree_stop_fence(session_id, first_fence),
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
            Some(first_fence),
        )?;
        transaction.commit()?;
        Ok(settlement)
    }

    #[cfg(test)]
    pub(crate) fn pending_agent_trigger_history_item_id(
        &self,
        session_id: SessionId,
    ) -> Result<Option<HistoryItemId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        pending_agent_trigger_history_item_id_in_connection(&connection, session_id, None)
    }

    pub(crate) fn pending_agent_trigger_history_item_id_for_tree_stop(
        &self,
        session_id: SessionId,
        fence: AgentTreeStopFence,
    ) -> Result<Option<HistoryItemId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        pending_agent_trigger_history_item_id_in_connection(&connection, session_id, Some(fence))
    }

    pub fn schedulable_owner_resume_request_id(
        &self,
        owner_session_id: SessionId,
    ) -> Result<Option<OwnerResumeRequestId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        schedulable_owner_resume_request_id_in_connection(&connection, owner_session_id)
    }

    pub fn list_pending_owner_resume_requests(
        &self,
        owner_session_id: SessionId,
    ) -> Result<Vec<OwnerResumeRequest>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        list_owner_resume_requests_in_connection(&connection, owner_session_id, "pending")
    }

    pub(crate) fn settle_pending_owner_resume_with_terminal(
        &self,
        owner_session_id: SessionId,
        expected_request_id: OwnerResumeRequestId,
        terminal: DurableTurnTerminal,
    ) -> Result<PendingAgentTriggerSettlement, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let settlement = settle_pending_owner_resume_in_transaction(
            &transaction,
            owner_session_id,
            expected_request_id,
            now,
            terminal,
        )?;
        transaction.commit()?;
        Ok(settlement)
    }

    pub async fn admit_active_goal_continuation_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<ActiveGoalTurnAdmission, StorageError> {
        self.admit_active_goal_continuation_turn_with_initial_user_turn(session_id, turn_id, None)
            .await
    }

    pub async fn admit_active_goal_continuation_turn_with_initial_user_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        initial_user_turn: Option<&UserTurn>,
    ) -> Result<ActiveGoalTurnAdmission, StorageError> {
        self.admit_session_turn_request_at(
            session_id,
            TurnAdmissionRequest::require_active_goal(turn_id, initial_user_turn),
            SystemClock::now_ms(),
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
    }

    pub async fn admit_session_turn_with_goal_objective(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        objective: impl Into<String>,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        self.admit_session_turn_with_goal_objective_and_initial_user_turn(
            session_id, turn_id, objective, None,
        )
        .await
    }

    pub async fn admit_session_turn_with_goal_objective_and_initial_user_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        objective: impl Into<String>,
        initial_user_turn: Option<&UserTurn>,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::set_goal_objective(turn_id, objective, initial_user_turn),
                SystemClock::now_ms(),
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await?
        {
            ActiveGoalTurnAdmission::Admitted(snapshot) => Ok(Some(snapshot)),
            ActiveGoalTurnAdmission::Unavailable => Ok(None),
            ActiveGoalTurnAdmission::GoalInactive => {
                unreachable!("goal-setting admission cannot reject an inactive goal")
            }
        }
    }

    pub async fn admit_session_turn_at(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::preserve_goal(turn_id, None),
                now_ms,
                lease_duration_ms,
            )
            .await?
        {
            ActiveGoalTurnAdmission::Admitted(snapshot) => Ok(Some(snapshot)),
            ActiveGoalTurnAdmission::Unavailable => Ok(None),
            ActiveGoalTurnAdmission::GoalInactive => {
                unreachable!("unconditional admission cannot reject an inactive goal")
            }
        }
    }

    async fn admit_session_turn_request_at(
        &self,
        session_id: SessionId,
        request: TurnAdmissionRequest,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<ActiveGoalTurnAdmission, StorageError> {
        if let TurnGoalAdmissionChange::SetObjective(objective) = &request.goal_change {
            validate_goal_objective_and_budget(objective, None)?;
        }
        if let Some(initial_user_turn) = request.initial_user_turn.as_ref() {
            if initial_user_turn.turn_id != request.turn_id {
                return Err(StorageError::Message(format!(
                    "initial user turn identity mismatch: payload turn {} admission turn {}",
                    initial_user_turn.turn_id, request.turn_id
                )));
            }
            if !initial_user_turn.items.iter().any(|item| match item {
                crate::protocol::UserInputItem::Text { text } => !text.trim().is_empty(),
                crate::protocol::UserInputItem::Image { .. } => true,
            }) {
                return Err(StorageError::Message(
                    "initial user turn must contain text or an image".to_string(),
                ));
            }
        }
        if (request.expected_agent_trigger_history_item_id.is_some()
            || request.expected_owner_resume_request_id.is_some())
            && request.initial_user_turn.is_some()
        {
            return Err(StorageError::Message(
                "durable agent-wake admission cannot also append an initial user turn".to_string(),
            ));
        }
        if request.expected_agent_trigger_history_item_id.is_some()
            && request.expected_owner_resume_request_id.is_some()
        {
            return Err(StorageError::Message(
                "one turn cannot claim both an explicit agent trigger and an owner resume"
                    .to_string(),
            ));
        }
        let admission_id = AdmissionId::new();
        let now = normalize_run_lease_now_ms(now_ms);
        let lease_expires_at_ms = run_lease_expiry_ms(now, lease_duration_ms);
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(runtime_state) = session_runtime_state_from_connection(&transaction, session_id)?
        else {
            transaction.commit()?;
            return Ok(ActiveGoalTurnAdmission::Unavailable);
        };
        if let Some(durable_admission) = runtime_state.admission {
            if durable_admission.is_fresh_at(now) {
                transaction.commit()?;
                return Ok(ActiveGoalTurnAdmission::Unavailable);
            }
            recover_expired_run_admission_in_transaction(
                &transaction,
                session_id,
                runtime_state.status,
                durable_admission,
                now,
            )?;
        }
        let pending_deferred = deferred_agent_completion_in_connection(
            &transaction,
            session_id,
            None,
            Some("pending"),
        )?;
        let owner_resume_can_recover_crash = request.expected_owner_resume_request_id.is_some()
            && pending_deferred
                .as_ref()
                .is_some_and(|deferred| deferred.kind == DeferredAgentCompletionKind::CrashFailed);
        let explicit_trigger_can_recover_crash =
            request.expected_agent_trigger_history_item_id.is_some()
                && pending_deferred.as_ref().is_some_and(|deferred| {
                    deferred.kind == DeferredAgentCompletionKind::CrashFailed
                });
        if pending_deferred.is_some()
            && !owner_resume_can_recover_crash
            && !explicit_trigger_can_recover_crash
        {
            transaction.commit()?;
            return Ok(ActiveGoalTurnAdmission::Unavailable);
        }
        if let Some(expected_history_item_id) = request.expected_agent_trigger_history_item_id
            && !pending_agent_trigger_is_unclaimed_in_transaction(
                &transaction,
                session_id,
                expected_history_item_id,
                false,
            )?
        {
            transaction.commit()?;
            return Ok(ActiveGoalTurnAdmission::Unavailable);
        }
        if let Some(expected_request_id) = request.expected_owner_resume_request_id
            && schedulable_owner_resume_request_id_in_connection(&transaction, session_id)?
                != Some(expected_request_id)
        {
            transaction.commit()?;
            return Ok(ActiveGoalTurnAdmission::Unavailable);
        }
        if request.goal_requirement == TurnGoalAdmissionRequirement::Active {
            let active_goal = stored_thread_goal_from_connection(&transaction, session_id)?
                .filter(|stored| stored.goal.status == ThreadGoalStatus::Active);
            if active_goal.is_none() {
                transaction.commit()?;
                return Ok(ActiveGoalTurnAdmission::GoalInactive);
            }
        }
        ensure_turn_identity_unused_in_transaction(&transaction, session_id, request.turn_id)?;
        let admitted = transaction.execute(
            "UPDATE sessions
             SET status = 'running',
                 updated_at_ms = ?2,
                  completed_at_ms = NULL,
                  active_run_id = ?3,
                  active_turn_id = ?4,
                  active_run_lease_expires_at_ms = ?5
              WHERE id = ?1
                AND active_run_id IS NULL
                AND status IN ('idle', 'completed', 'cancelled', 'failed')",
            params![
                session_id.to_string(),
                now,
                admission_id.to_string(),
                request.turn_id.to_string(),
                lease_expires_at_ms
            ],
        )? == 1;
        if !admitted {
            transaction.commit()?;
            return Ok(ActiveGoalTurnAdmission::Unavailable);
        }
        if let TurnGoalAdmissionChange::SetObjective(objective) = &request.goal_change {
            set_thread_goal_objective_in_transaction(&transaction, session_id, objective, now)?;
        }
        let session_title = transaction.query_row(
            "SELECT title FROM sessions WHERE id = ?1",
            params![session_id.to_string()],
            |row| row.get::<_, String>(0),
        )?;
        let started = RunEvent::SessionStarted {
            session_id,
            title: session_title,
        };
        let started_projection =
            project_protocol_run_event(&started, Some(session_id), request.turn_id, 0).ok_or_else(
                || {
                    StorageError::Message(
                        "SessionStarted did not produce a protocol bundle".to_string(),
                    )
                },
            )?;
        insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            &transaction,
            &started_projection.runtime_event,
            started_projection.history_item.as_ref(),
            started_projection.turn_item.as_ref(),
        )?;
        if let Some(expected_history_item_id) = request.expected_agent_trigger_history_item_id {
            insert_agent_trigger_turn_claim_in_transaction(
                &transaction,
                session_id,
                admission_id,
                request.turn_id,
                expected_history_item_id,
                now,
            )?;
        }
        if let Some(expected_request_id) = request.expected_owner_resume_request_id {
            let claimed = claim_pending_owner_resume_requests_in_transaction(
                &transaction,
                session_id,
                request.turn_id,
                expected_request_id,
                now,
            )?;
            if claimed == 0 {
                return Err(StorageError::Message(format!(
                    "owner-resume admission for session {session_id} lost request {expected_request_id} after SessionStarted"
                )));
            }
        } else if request.expected_agent_trigger_history_item_id.is_some()
            && let Some(pending_request_id) =
                oldest_pending_owner_resume_request_id_in_connection(&transaction, session_id)?
        {
            // Explicit owner work wins the wake projection, but that same newly-admitted turn
            // resumes ownership and therefore coalesces every already-pending resume request.
            claim_pending_owner_resume_requests_in_transaction(
                &transaction,
                session_id,
                request.turn_id,
                pending_request_id,
                now,
            )?;
        }
        let initial_user_history_item_id = if let Some(initial_user_turn) =
            request.initial_user_turn.as_ref()
        {
            let stored = RunEvent::UserTurnStored {
                session_id,
                turn: Box::new(initial_user_turn.clone()),
            };
            let stored_projection =
                project_protocol_run_event(&stored, Some(session_id), request.turn_id, 1)
                    .ok_or_else(|| {
                        StorageError::Message(
                            "UserTurnStored did not produce a protocol bundle".to_string(),
                        )
                    })?;
            let stored = insert_session_owned_event_bundle_in_transaction(
                &SESSION_PROTOCOL_WRITE_AUTHORITY,
                &transaction,
                &stored_projection.runtime_event,
                stored_projection.history_item.as_ref(),
                stored_projection.turn_item.as_ref(),
            )?;
            let history_item = stored.history_item.ok_or_else(|| {
                StorageError::Message(
                    "UserTurnStored protocol bundle omitted its canonical history item".to_string(),
                )
            })?;
            Some(history_item.id)
        } else {
            None
        };
        let goal = stored_thread_goal_from_connection(&transaction, session_id)?.map(|stored| {
            AdmittedThreadGoal {
                goal_id: stored.goal_id,
                goal: stored.goal,
            }
        });
        transaction.commit()?;
        Ok(ActiveGoalTurnAdmission::Admitted(AdmittedTurnSnapshot {
            admission_id,
            goal,
            initial_user_history_item_id,
        }))
    }

    pub async fn renew_admitted_run_lease(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<RunAdmissionLeaseRenewalOutcome, StorageError> {
        self.renew_admitted_run_lease_at(
            session_id,
            admission_id,
            turn_id,
            SystemClock::now_ms(),
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
    }

    pub async fn renew_admitted_run_lease_at(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<RunAdmissionLeaseRenewalOutcome, StorageError> {
        let now = normalize_run_lease_now_ms(now_ms);
        let requested_expiry = run_lease_expiry_ms(now, lease_duration_ms);
        let admission_id_text = admission_id.to_string();
        let turn_id_text = turn_id.to_string();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = session_runtime_state_from_connection(&transaction, session_id)?;
        let outcome = match state {
            Some(runtime_state) if runtime_state.status == SessionStatus::Running => {
                let active_admission = runtime_state
                    .admission
                    .expect("running session admission validated before lease renewal");
                if !active_admission.is_fresh_at(now)
                    || active_admission.admission_id != admission_id
                    || active_admission.turn_id != turn_id
                {
                    RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                } else if let Some(fence) = first_applicable_tree_stop_fence_for_turn_in_connection(
                    &transaction,
                    session_id,
                    turn_id,
                )? {
                    RunAdmissionLeaseRenewalOutcome::StopFenced(
                        recovery_terminal_outcome_for_tree_stop_fence(session_id, fence),
                    )
                } else {
                    let renewed = transaction.execute(
                        "UPDATE sessions
                         SET active_run_lease_expires_at_ms = MAX(
                                 active_run_lease_expires_at_ms,
                                 ?4
                             )
                          WHERE id = ?1
                            AND active_run_id = ?2
                            AND active_turn_id = ?3
                            AND active_run_lease_expires_at_ms > ?5
                            AND status = 'running'",
                        params![
                            session_id.to_string(),
                            admission_id_text,
                            turn_id_text,
                            requested_expiry,
                            now
                        ],
                    )?;
                    if renewed == 1 {
                        RunAdmissionLeaseRenewalOutcome::Renewed
                    } else {
                        RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                    }
                }
            }
            Some(runtime_state)
                if matches!(
                    runtime_state.status,
                    SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
                ) =>
            {
                let retained_admission = runtime_state.admission;
                if let Some(retained_admission) = retained_admission {
                    let terminal = terminal_for_retained_admission_in_connection(
                        &transaction,
                        session_id,
                        runtime_state.status,
                        retained_admission,
                    )?;
                    if retained_admission.admission_id == admission_id
                        && retained_admission.turn_id == turn_id
                    {
                        RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                    } else {
                        RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                    }
                } else if let Some(terminal) =
                    terminal_for_turn_in_connection(&transaction, session_id, turn_id)?
                {
                    RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                } else {
                    RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                }
            }
            _ => RunAdmissionLeaseRenewalOutcome::SupersededOrExpired,
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub async fn admitted_run_status(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<Option<SessionStatus>, StorageError> {
        self.admitted_run_status_at(session_id, admission_id, turn_id, SystemClock::now_ms())
            .await
    }

    pub async fn admitted_run_status_at(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        now_ms: i64,
    ) -> Result<Option<SessionStatus>, StorageError> {
        Ok(
            match self
                .admitted_run_state_at(session_id, admission_id, turn_id, now_ms)
                .await?
            {
                AdmittedRunState::OwnedRunning => Some(SessionStatus::Running),
                AdmittedRunState::Terminal(terminal) => Some(terminal.session_status()),
                AdmittedRunState::StopFenced(_) | AdmittedRunState::SupersededOrExpired => None,
            },
        )
    }

    pub(crate) async fn admitted_run_state(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<AdmittedRunState, StorageError> {
        self.admitted_run_state_at(session_id, admission_id, turn_id, SystemClock::now_ms())
            .await
    }

    pub(crate) async fn admitted_run_state_at(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        now_ms: i64,
    ) -> Result<AdmittedRunState, StorageError> {
        let now = normalize_run_lease_now_ms(now_ms);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(AdmittedRunState::SupersededOrExpired);
        };
        if runtime_state.status == SessionStatus::Running {
            let Some(admission) = runtime_state.fresh_admission_at(now).filter(|admission| {
                admission.admission_id == admission_id && admission.turn_id == turn_id
            }) else {
                return Ok(AdmittedRunState::SupersededOrExpired);
            };
            debug_assert_eq!(admission.turn_id, turn_id);
            if let Some(fence) = first_applicable_tree_stop_fence_for_turn_in_connection(
                &connection,
                session_id,
                turn_id,
            )? {
                return Ok(AdmittedRunState::StopFenced(
                    recovery_terminal_outcome_for_tree_stop_fence(session_id, fence),
                ));
            }
            return Ok(AdmittedRunState::OwnedRunning);
        }
        if matches!(
            runtime_state.status,
            SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
        ) && let Some(terminal) =
            terminal_for_turn_in_connection(&connection, session_id, turn_id)?
        {
            return Ok(AdmittedRunState::Terminal(terminal));
        }
        Ok(AdmittedRunState::SupersededOrExpired)
    }

    pub async fn durable_terminal_for_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<crate::session::model::DurableTurnTerminal>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let session_exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            params![session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?;
        if !session_exists {
            transaction.commit()?;
            return Ok(None);
        }
        let protocol_terminal = terminal_for_turn_in_connection(&transaction, session_id, turn_id)?;
        transaction.commit()?;
        Ok(protocol_terminal)
    }

    pub async fn latest_durable_terminal_before_turn(
        &self,
        session_id: SessionId,
        current_turn_id: TurnId,
    ) -> Result<Option<DurableTurnTerminal>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let previous_turn_id = transaction
            .query_row(
                "SELECT runtime_event.turn_id
                 FROM protocol_runtime_events AS runtime_event
                 JOIN protocol_item_append_order AS append_order
                   ON append_order.session_id = runtime_event.session_id
                  AND append_order.source_kind = 'runtime_event'
                  AND append_order.source_id = runtime_event.id
                 WHERE runtime_event.session_id = ?1
                   AND runtime_event.turn_id <> ?2
                   AND json_extract(runtime_event.msg_json, '$.kind') = 'turn_terminal'
                   AND append_order.append_position < COALESCE(
                       (SELECT MIN(current_order.append_position)
                        FROM protocol_item_append_order AS current_order
                        WHERE current_order.session_id = ?1
                          AND current_order.turn_id = ?2),
                       9223372036854775807
                   )
                 ORDER BY append_order.append_position DESC
                 LIMIT 1",
                params![session_id.to_string(), current_turn_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let terminal = match previous_turn_id {
            Some(turn_id) => {
                let turn_id = turn_id.parse::<TurnId>().map_err(|_| {
                    StorageError::Message(format!(
                        "session {session_id} has an invalid prior terminal turn identity"
                    ))
                })?;
                terminal_for_turn_in_connection(&transaction, session_id, turn_id)?
            }
            None => None,
        };
        transaction.commit()?;
        Ok(terminal)
    }

    pub async fn has_fresh_run_admission(
        &self,
        session_id: SessionId,
    ) -> Result<bool, StorageError> {
        self.has_fresh_run_admission_at(session_id, SystemClock::now_ms())
            .await
    }

    pub async fn has_fresh_run_admission_at(
        &self,
        session_id: SessionId,
        now_ms: i64,
    ) -> Result<bool, StorageError> {
        let now = normalize_run_lease_now_ms(now_ms);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(false);
        };
        let Some(admission) = runtime_state.fresh_admission_at(now) else {
            return Ok(false);
        };
        Ok(
            !turn_started_before_applicable_tree_stop_fence_in_transaction(
                &connection,
                session_id,
                admission.turn_id,
            )?,
        )
    }

    pub async fn fresh_running_turn_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<TurnId>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(None);
        };
        let Some(turn_id) = runtime_state.fresh_running_turn_at(now) else {
            return Ok(None);
        };
        Ok(
            (!turn_started_before_applicable_tree_stop_fence_in_transaction(
                &connection,
                session_id,
                turn_id,
            )?)
            .then_some(turn_id),
        )
    }

    pub async fn session_blocks_mutation(
        &self,
        session_id: SessionId,
    ) -> Result<bool, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(false);
        };
        Ok(runtime_state.blocks_mutation_at(now)
            && !runtime_state_admission_started_before_tree_stop_fence(
                &connection,
                session_id,
                runtime_state,
            )?)
    }

    pub(crate) async fn durable_session_stop_state(
        &self,
        session_id: SessionId,
    ) -> Result<Option<DurableSessionStopState>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        Ok(
            session_runtime_state_from_connection(&connection, session_id)?
                .map(ValidatedSessionRuntimeState::stop_state),
        )
    }

    pub(crate) async fn durable_session_stop_state_at_tree_stop_fence(
        &self,
        session_id: SessionId,
        fence: AgentTreeStopFence,
    ) -> Result<Option<DurableSessionStopState>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let belongs_to_fenced_scope = session_belongs_to_exact_tree_stop_fence_scope_in_connection(
            &connection,
            session_id,
            fence,
        )?;
        if !belongs_to_fenced_scope {
            return Ok(None);
        }
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(None);
        };
        let stop_state = runtime_state.stop_state();
        let DurableSessionStopState::Running(target) = stop_state else {
            return Ok(Some(stop_state));
        };
        let turn_started_before_fence = connection.query_row(
            "SELECT COALESCE(MIN(append_position) <= ?3, 0)
             FROM protocol_item_append_order
             WHERE session_id = ?1 AND turn_id = ?2",
            params![
                session_id.to_string(),
                target.turn_id().to_string(),
                fence.after_append_position,
            ],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(turn_started_before_fence.then_some(DurableSessionStopState::Running(target)))
    }

    pub(crate) async fn tree_stop_interruption_cause_for_running_target_at_fence(
        &self,
        session_id: SessionId,
        target: RunningSessionTerminalTarget,
        fence: AgentTreeStopFence,
    ) -> Result<Option<crate::protocol::TurnInterruptionCause>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let belongs_to_requested_fence =
            session_belongs_to_exact_tree_stop_fence_scope_in_connection(
                &connection,
                session_id,
                fence,
            )?;
        if !belongs_to_requested_fence {
            return Ok(None);
        }
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(None);
        };
        let Some(admission) = runtime_state.admission else {
            return Ok(None);
        };
        if runtime_state.status != SessionStatus::Running || !target.matches(admission) {
            return Ok(None);
        }
        let Some(first_fence) = first_applicable_tree_stop_fence_for_turn_in_connection(
            &connection,
            session_id,
            target.turn_id(),
        )?
        else {
            return Ok(None);
        };
        if first_fence.root_session_id != fence.root_session_id
            || first_fence.after_append_position > fence.after_append_position
        {
            return Ok(None);
        }
        Ok(tree_stop_interruption_cause_for_fence(
            session_id,
            first_fence,
        ))
    }

    #[cfg(test)]
    pub(crate) async fn captured_running_terminal_target(
        &self,
        session_id: SessionId,
    ) -> Result<Option<RunningSessionTerminalTarget>, StorageError> {
        Ok(match self.durable_session_stop_state(session_id).await? {
            Some(DurableSessionStopState::Running(target)) => Some(target),
            Some(DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_)) | None => {
                None
            }
        })
    }

    pub async fn mutation_blocker_in_session_tree(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        active_session_for_mutation_branch(&connection, session_id, true)
    }

    pub async fn release_stopped_run_admission(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
    ) -> Result<bool, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = session_runtime_state_from_connection(&transaction, session_id)?;
        let released = match state {
            Some(runtime_state)
                if runtime_state.status != SessionStatus::Running
                    && runtime_state.admission.is_some() =>
            {
                let admission = runtime_state
                    .admission
                    .expect("terminal retained admission matched above");
                if admission.admission_id != admission_id {
                    false
                } else {
                    transaction.execute(
                        "UPDATE sessions
                         SET active_run_id = NULL,
                             active_turn_id = NULL,
                             active_run_lease_expires_at_ms = NULL
                         WHERE id = ?1
                           AND active_run_id = ?2
                           AND active_turn_id = ?3
                           AND active_run_lease_expires_at_ms = ?4
                           AND status != 'running'",
                        params![
                            session_id.to_string(),
                            admission.admission_id.to_string(),
                            admission.turn_id.to_string(),
                            admission.lease_expires_at_ms,
                        ],
                    )? == 1
                }
            }
            _ => false,
        };
        transaction.commit()?;
        Ok(released)
    }

    pub async fn accept_active_turn_steer(
        &self,
        session_id: SessionId,
        steer: &SteerTurn,
    ) -> Result<HistoryItemId, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let runtime_state = session_runtime_state_from_connection(&transaction, session_id)?
            .ok_or_else(|| StorageError::Message(format!("session {session_id} was not found")))?;
        if runtime_state.status != SessionStatus::Running {
            return Err(StorageError::Message(format!(
                "no active running turn to steer for session {session_id}; current status is {}",
                runtime_state.status.key()
            )));
        }
        let durable_admission = runtime_state
            .admission
            .expect("running steer target admission validated before freshness check");
        if !durable_admission.is_fresh_at(now) {
            return Err(StorageError::Message(format!(
                "run admission lease expired for session {session_id}"
            )));
        }
        let active_turn_id = durable_admission.turn_id;
        if turn_started_before_applicable_tree_stop_fence_in_transaction(
            &transaction,
            session_id,
            active_turn_id,
        )? {
            return Err(StorageError::Message(format!(
                "active turn {active_turn_id} for session {session_id} was closed by a durable tree Stop"
            )));
        }
        if active_turn_id != steer.expected_turn_id {
            return Err(StorageError::Message(format!(
                "expected active turn id `{}` but current active turn id is `{active_turn_id}`",
                steer.expected_turn_id
            )));
        }

        let input_id = HistoryItemId::new();
        let payload_json = serde_json::to_string(&HistoryItemPayload::SteerTurn {
            expected_turn_id: active_turn_id,
            content: steer.content_parts(),
            additional_context: steer.additional_context.clone(),
            client_user_message_id: steer.client_user_message_id.clone(),
        })?;
        let payload_sha256 = sha256_payload(&payload_json);
        transaction.execute(
            "INSERT INTO turn_steer_inputs (
                 id,
                 session_id,
                 admission_id,
                 turn_id,
                 payload_json,
                 payload_sha256,
                 origin_kind,
                 state,
                 delivered_history_item_id,
                 resolved_by_terminal_event_id,
                 accepted_at_ms,
                 delivered_at_ms,
                 discarded_at_ms,
                 updated_at_ms
             )
             VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, 'runtime', 'queued',
                 NULL, NULL, ?7, NULL, NULL, ?7
             )",
            params![
                input_id.to_string(),
                session_id.to_string(),
                durable_admission.admission_id.to_string(),
                active_turn_id.to_string(),
                payload_json,
                payload_sha256,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(input_id)
    }

    pub(crate) fn deliver_all_pending_turn_steers_for_admitted_turn(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<Vec<HistoryItemId>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(&transaction, session_id, admission_id, turn_id)?;
        let delivered = deliver_all_pending_turn_steers_in_transaction(
            &transaction,
            session_id,
            admission_id,
            turn_id,
            now,
        )?;
        transaction.commit()?;
        Ok(delivered)
    }

    pub(crate) fn has_pending_turn_steers_for_admitted_turn(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<bool, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(&transaction, session_id, admission_id, turn_id)?;
        let pending = count_pending_turn_steers_in_transaction(
            &transaction,
            session_id,
            admission_id,
            turn_id,
        )? > 0;
        transaction.commit()?;
        Ok(pending)
    }

    pub async fn active_session_for_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<SessionId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        mutation_blocker_for_project_in_connection(&connection, project_id)
    }

    pub(crate) async fn terminalize_captured_running_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, StorageError> {
        Ok(self
            .terminalize_turn_with_protocol_event_guarded(
                session_id,
                event,
                TerminalOwnerGuard::Captured(target),
                None,
                true,
                false,
                true,
                None,
            )?
            .admitted_commit()?
            .was_applied())
    }

    pub(crate) async fn recover_captured_running_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, StorageError> {
        Ok(self
            .terminalize_turn_with_protocol_event_guarded(
                session_id,
                event,
                TerminalOwnerGuard::Captured(target),
                None,
                false,
                true,
                true,
                None,
            )?
            .admitted_commit()?
            .was_applied())
    }

    pub(crate) async fn record_agent_tree_stop_fence(
        &self,
        stopped_session_id: SessionId,
        cause: crate::protocol::TurnInterruptionCause,
    ) -> Result<Option<AgentTreeStopFence>, StorageError> {
        let cause = explicit_agent_tree_stop_fence_cause(cause)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fence = record_agent_tree_stop_fence_in_transaction(
            &transaction,
            stopped_session_id,
            cause,
            now,
        )?;
        transaction.commit()?;
        Ok(fence)
    }

    pub(crate) async fn record_agent_tree_stop_fence_for_observed_turn(
        &self,
        stopped_session_id: SessionId,
        cause: crate::protocol::TurnInterruptionCause,
        observed_turn_id: TurnId,
    ) -> Result<Option<AgentTreeStopFence>, StorageError> {
        let cause = explicit_agent_tree_stop_fence_cause(cause)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if turn_start_append_position_in_connection(
            &transaction,
            stopped_session_id,
            observed_turn_id,
        )?
        .is_none()
        {
            transaction.commit()?;
            return Err(StorageError::Message(format!(
                "observed tree-stop turn {observed_turn_id} does not belong to session {stopped_session_id}"
            )));
        }
        if let Some(existing) = first_applicable_tree_stop_fence_for_turn_in_connection(
            &transaction,
            stopped_session_id,
            observed_turn_id,
        )? {
            let fence = AgentTreeStopFence {
                root_session_id: existing.root_session_id,
                stopped_session_id: existing.stopped_session_id,
                after_append_position: existing.after_append_position,
            };
            transaction.commit()?;
            return Ok(Some(fence));
        }
        let fence = record_agent_tree_stop_fence_in_transaction(
            &transaction,
            stopped_session_id,
            cause,
            now,
        )?;
        transaction.commit()?;
        Ok(fence)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn terminalize_admitted_turn_with_protocol_event(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        self.terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
            session_id,
            admission_id,
            event,
            protocol_turn_id,
            protocol_sequence_no,
            true,
            expected_active_goal_id_to_block,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        accepts_mailbox_delivery_current_turn: bool,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        self.terminalize_turn_with_protocol_event_guarded(
            session_id,
            event,
            TerminalOwnerGuard::Admitted {
                admission_id,
                turn_id: protocol_turn_id,
            },
            protocol_sequence_no,
            false,
            false,
            accepts_mailbox_delivery_current_turn,
            expected_active_goal_id_to_block,
        )?
        .admitted_commit()
    }

    pub(crate) fn settle_agent_execution_wake_with_terminal(
        &self,
        session_id: SessionId,
        wake: AgentExecutionWakeTerminalOwner,
        terminal: DurableTurnTerminal,
    ) -> Result<AgentExecutionWakeTerminalSettlement, StorageError> {
        if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            return Err(StorageError::Message(
                "agent execution wake settlement only accepts an interrupted terminal".to_string(),
            ));
        }
        let event = RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(terminal),
        };
        let settlement = self.terminalize_turn_with_protocol_event_guarded(
            session_id,
            &event,
            TerminalOwnerGuard::AgentWake(wake),
            None,
            false,
            false,
            false,
            None,
        )?;
        Ok(match settlement {
            GuardedTerminalization::Settled {
                commit: AdmittedTerminalCommit::Applied,
                turn_id,
                terminal,
            } => AgentExecutionWakeTerminalSettlement::Applied { turn_id, terminal },
            GuardedTerminalization::Settled {
                commit:
                    AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission
                    | AdmittedTerminalCommit::NotOwned,
                turn_id,
                terminal,
            } => AgentExecutionWakeTerminalSettlement::AlreadyTerminal { turn_id, terminal },
            GuardedTerminalization::BlockedByPendingDeferredCompletion { deferred_turn_id } => {
                AgentExecutionWakeTerminalSettlement::BlockedByPendingDeferredCompletion {
                    deferred_turn_id,
                }
            }
            GuardedTerminalization::NotOwned => {
                AgentExecutionWakeTerminalSettlement::WakeUnavailable
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn terminalize_turn_with_protocol_event_guarded(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        owner_guard: TerminalOwnerGuard,
        protocol_sequence_no: Option<i64>,
        retain_active_admission: bool,
        orphan_recovery: bool,
        accepts_mailbox_delivery_current_turn: bool,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<GuardedTerminalization, StorageError> {
        let requested_terminal = validate_terminal_event(session_id, event)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let Some(runtime_state) = session_runtime_state_from_connection(&transaction, session_id)?
        else {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        };
        let wake_claim = match owner_guard {
            TerminalOwnerGuard::AgentWake(AgentExecutionWakeTerminalOwner::ExplicitTask(
                history_item_id,
            )) => {
                agent_trigger_turn_claim_in_connection(&transaction, session_id, history_item_id)?
                    .map(|(admission_id, turn_id)| (Some(admission_id), turn_id))
            }
            TerminalOwnerGuard::AgentWake(AgentExecutionWakeTerminalOwner::OwnerResume(
                request_id,
            )) => owner_resume_claimed_turn_in_connection(&transaction, session_id, request_id)?
                .map(|turn_id| (None, turn_id)),
            TerminalOwnerGuard::Admitted { .. } | TerminalOwnerGuard::Captured(_) => None,
        };
        if let TerminalOwnerGuard::AgentWake(wake) = owner_guard
            && wake_claim.is_none()
        {
            let settlement = match wake {
                AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id) => {
                    settle_pending_agent_trigger_in_transaction(
                        &transaction,
                        session_id,
                        history_item_id,
                        now,
                        requested_terminal.clone(),
                        None,
                    )?
                }
                AgentExecutionWakeTerminalOwner::OwnerResume(request_id) => {
                    settle_pending_owner_resume_in_transaction(
                        &transaction,
                        session_id,
                        request_id,
                        now,
                        requested_terminal.clone(),
                    )?
                }
            };
            let result = match settlement {
                PendingAgentTriggerSettlement::Applied { turn_id, .. } => {
                    let terminal = terminal_for_turn_in_connection(
                        &transaction,
                        session_id,
                        turn_id,
                    )?
                    .ok_or_else(|| {
                        StorageError::Message(format!(
                            "applied agent wake settlement omitted terminal for turn {turn_id}"
                        ))
                    })?;
                    GuardedTerminalization::Settled {
                        commit: AdmittedTerminalCommit::Applied,
                        turn_id,
                        terminal,
                    }
                }
                PendingAgentTriggerSettlement::WakeOwnedOrResolved => {
                    GuardedTerminalization::NotOwned
                }
                PendingAgentTriggerSettlement::BlockedByPendingDeferredCompletion {
                    deferred_turn_id,
                } => {
                    GuardedTerminalization::BlockedByPendingDeferredCompletion { deferred_turn_id }
                }
            };
            transaction.commit()?;
            return Ok(result);
        }
        let Some(durable_admission) = runtime_state.admission else {
            let settled = match wake_claim {
                Some((_, turn_id)) => {
                    terminal_for_turn_in_connection(&transaction, session_id, turn_id)?
                        .map(|terminal| (turn_id, terminal))
                }
                None => None,
            };
            transaction.commit()?;
            return Ok(match settled {
                Some((turn_id, terminal)) => GuardedTerminalization::Settled {
                    commit: AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission,
                    turn_id,
                    terminal,
                },
                None => GuardedTerminalization::NotOwned,
            });
        };
        let admitted_guard = matches!(
            owner_guard,
            TerminalOwnerGuard::Admitted { .. } | TerminalOwnerGuard::AgentWake(_)
        );
        let owner_matches = match owner_guard {
            TerminalOwnerGuard::Admitted {
                admission_id,
                turn_id,
            } => {
                durable_admission.admission_id == admission_id
                    && durable_admission.turn_id == turn_id
                    && durable_admission.is_fresh_at(now)
            }
            TerminalOwnerGuard::Captured(target) => target.matches(durable_admission),
            TerminalOwnerGuard::AgentWake(_) => {
                wake_claim.is_some_and(|(expected_admission_id, expected_turn_id)| {
                    expected_turn_id == durable_admission.turn_id
                        && expected_admission_id.is_none_or(|admission_id| {
                            admission_id == durable_admission.admission_id
                        })
                })
            }
        };
        if !owner_matches {
            let settled = match wake_claim {
                Some((_, turn_id)) => {
                    terminal_for_turn_in_connection(&transaction, session_id, turn_id)?
                        .map(|terminal| (turn_id, terminal))
                }
                None => None,
            };
            transaction.commit()?;
            return Ok(match settled {
                Some((turn_id, terminal)) => GuardedTerminalization::Settled {
                    commit: AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission,
                    turn_id,
                    terminal,
                },
                None => GuardedTerminalization::NotOwned,
            });
        }
        let protocol_turn_id = durable_admission.turn_id;
        let admission_id_text = durable_admission.admission_id.to_string();
        let applicable_tree_stop_fence = first_applicable_tree_stop_fence_for_turn_in_connection(
            &transaction,
            session_id,
            protocol_turn_id,
        )?;
        if matches!(
            requested_terminal.outcome,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::TreeStopped
            }
        ) && applicable_tree_stop_fence.is_none()
        {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }
        let reconstructed_wake_event = if matches!(owner_guard, TerminalOwnerGuard::AgentWake(_))
            && matches!(
                requested_terminal.outcome,
                TurnTerminalOutcome::Interrupted { .. }
            ) {
            let snapshot =
                canonical_turn_snapshot_in_transaction(&transaction, session_id, protocol_turn_id)?;
            let mut terminal = requested_terminal.clone();
            terminal.final_response_id = None;
            terminal.tool_call_count = snapshot.tool_call_count;
            terminal.failed_tool_count = snapshot.failed_tool_count;
            terminal.change_count = snapshot.change_count;
            terminal.metrics = Default::default();
            Some(RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(terminal),
            })
        } else {
            None
        };
        let requested_event = reconstructed_wake_event.as_ref().unwrap_or(event);
        let requested_terminal = validate_terminal_event(session_id, requested_event)?;
        let recovery_event = if orphan_recovery {
            applicable_tree_stop_fence.map(|fence| {
                let mut terminal = requested_terminal.clone();
                terminal.outcome = recovery_terminal_outcome_for_tree_stop_fence(session_id, fence);
                terminal.final_response_id = None;
                RunEvent::TurnTerminal {
                    session_id,
                    terminal: Box::new(terminal),
                }
            })
        } else {
            None
        };
        let event = recovery_event.as_ref().unwrap_or(requested_event);
        let terminal = validate_terminal_event(session_id, event)?;
        let status = terminal.session_status();
        if let Some(fence) = applicable_tree_stop_fence
            && !terminal_is_compatible_with_tree_stop_fence(session_id, terminal, fence)
        {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }
        if runtime_state.status != SessionStatus::Running {
            terminal_for_retained_admission_in_connection(
                &transaction,
                session_id,
                runtime_state.status,
                durable_admission,
            )?;
            let actual_terminal = terminal_for_turn_in_connection(
                &transaction,
                session_id,
                protocol_turn_id,
            )?
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "retained terminal owner for turn {protocol_turn_id} has no durable terminal"
                ))
            })?;
            if admitted_guard {
                transaction.execute(
                    "UPDATE sessions
                     SET active_run_id = NULL,
                         active_turn_id = NULL,
                         active_run_lease_expires_at_ms = NULL
                     WHERE id = ?1
                       AND active_run_id = ?2
                       AND active_turn_id = ?3
                       AND active_run_lease_expires_at_ms = ?4",
                    params![
                        session_id.to_string(),
                        admission_id_text,
                        protocol_turn_id.to_string(),
                        durable_admission.lease_expires_at_ms,
                    ],
                )?;
                transaction.commit()?;
                return Ok(GuardedTerminalization::Settled {
                    commit: AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission,
                    turn_id: protocol_turn_id,
                    terminal: actual_terminal,
                });
            }
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }
        if terminal_for_turn_in_connection(&transaction, session_id, protocol_turn_id)?.is_some() {
            return Err(StorageError::Message(format!(
                "running session {session_id} active turn {protocol_turn_id} already has a durable terminal"
            )));
        }
        let terminal_resolves_owner_resume = transaction.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM agent_owner_resume_requests
                 WHERE owner_session_id = ?1
                   AND state = 'claimed'
                   AND claimed_turn_id = ?2
             )",
            params![session_id.to_string(), protocol_turn_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?;
        let pending_deferred_before_terminal = deferred_agent_completion_in_connection(
            &transaction,
            session_id,
            None,
            Some("pending"),
        )?;
        let terminal_recovers_pending_crash = !orphan_recovery
            && pending_deferred_before_terminal
                .as_ref()
                .is_some_and(|deferred| deferred.kind == DeferredAgentCompletionKind::CrashFailed);

        let recovering_owner_resume = orphan_recovery && terminal_resolves_owner_resume;

        let pending_direct_child_result =
            pending_direct_child_result_terminal_in_connection(&transaction, session_id)?;
        let explicit_wake_claim = agent_trigger_history_item_for_turn_in_connection(
            &transaction,
            session_id,
            protocol_turn_id,
        )?;

        // Codex does not reject a model terminal and force another sample merely because input
        // raced the final response. Mail already eligible for this turn is atomically recorded in
        // canonical history before the terminal; mail assigned to the next-turn phase remains
        // pending for the next explicit turn. Direct-child FINAL messages follow the same rule.
        if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            // Admission already selected this one immutable wake. Even if setup fails before the
            // normal safe mailbox boundary, its Completed/Failed terminal must consume exactly
            // that input. Leaving it pending would make a later scheduler retry collide with the
            // immutable V53 claim. Mail accepted after this selected wake remains next-turn work
            // unless the regular current-turn delivery phase was reached.
            if let Some(history_item_id) = explicit_wake_claim {
                deliver_claimed_explicit_agent_wake_in_transaction(
                    &transaction,
                    session_id,
                    protocol_turn_id,
                    history_item_id,
                    now,
                )?;
            }
            if accepts_mailbox_delivery_current_turn {
                deliver_all_pending_agent_mail_in_transaction(
                    &transaction,
                    session_id,
                    protocol_turn_id,
                    now,
                )?;
            }
        }

        let retained_parent = retained_agent_parent_in_connection(&transaction, session_id)?;
        // Like Codex threads, every agent owns its own terminal independently of descendant
        // liveness. Waiting for a result is an explicit model/tool decision; a normal Completed
        // terminal neither waits for descendants nor defers its direct-parent handoff.
        //
        // Descendant liveness remains relevant only to crash recovery. A recovered non-root owner
        // may need one exact deferred failure generation so a retry can supersede it without
        // publishing a synthetic result upstream.
        let has_durable_descendant_work = orphan_recovery
            && retained_parent.is_some()
            && session_has_durable_descendant_work_in_connection(&transaction, session_id)?;
        let deferred_kind = retained_parent.and_then(|_| {
            if orphan_recovery
                && matches!(terminal.outcome, TurnTerminalOutcome::Failed { .. })
                && (has_durable_descendant_work
                    || pending_direct_child_result.is_some()
                    || recovering_owner_resume)
            {
                Some(DeferredAgentCompletionKind::CrashFailed)
            } else {
                None
            }
        });

        if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            // Codex records steering that won the narrow race after the
            // post-response pending-input check when the task finishes
            // normally or unexpectedly. It is history evidence, but it is not
            // sampled by another model request.
            deliver_all_pending_turn_steers_in_transaction(
                &transaction,
                session_id,
                durable_admission.admission_id,
                protocol_turn_id,
                now,
            )?;
        }

        let status_text = session_status_text(status);
        let clear_admission = !retain_active_admission;
        let terminalized = if clear_admission {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?5,
                     updated_at_ms = ?6,
                     completed_at_ms = ?6,
                     active_run_id = NULL,
                     active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1
                   AND active_run_id = ?2
                   AND active_turn_id = ?3
                   AND active_run_lease_expires_at_ms = ?4
                   AND status = 'running'",
                params![
                    session_id.to_string(),
                    admission_id_text,
                    protocol_turn_id.to_string(),
                    durable_admission.lease_expires_at_ms,
                    status_text,
                    now,
                ],
            )? == 1
        } else {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?5, updated_at_ms = ?6, completed_at_ms = ?6
                 WHERE id = ?1
                   AND active_run_id = ?2
                   AND active_turn_id = ?3
                   AND active_run_lease_expires_at_ms = ?4
                   AND status = 'running'",
                params![
                    session_id.to_string(),
                    admission_id_text,
                    protocol_turn_id.to_string(),
                    durable_admission.lease_expires_at_ms,
                    status_text,
                    now,
                ],
            )? == 1
        };
        if !terminalized {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }

        if status == SessionStatus::Failed
            && let Some(expected_goal_id) = expected_active_goal_id_to_block
        {
            transaction.execute(
                "UPDATE thread_goals
                 SET status = 'blocked', updated_at_ms = MAX(updated_at_ms + 1, ?3)
                 WHERE thread_id = ?1 AND goal_id = ?2 AND status = 'active'",
                params![session_id.to_string(), expected_goal_id, now],
            )?;
        }

        let protocol_sequence_no = resolve_terminal_protocol_sequence_in_transaction(
            &transaction,
            session_id,
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        let terminal_sequence_no = settle_unfinished_tool_calls_for_terminal_event(
            &transaction,
            session_id,
            event,
            protocol_turn_id,
            protocol_sequence_no,
            now,
        )?;
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            Some(terminal_sequence_no),
        )?;
        if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            let terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            if let Some(history_item_id) = explicit_wake_claim {
                discard_pending_explicit_agent_wake_in_transaction(
                    &transaction,
                    session_id,
                    history_item_id,
                    terminal_event_id,
                    now,
                )?;
            }
            discard_all_pending_turn_steers_in_transaction(
                &transaction,
                session_id,
                durable_admission.admission_id,
                protocol_turn_id,
                terminal_event_id,
                now,
            )?;
        }
        let terminal_is_tree_stop_fenced =
            turn_started_before_applicable_tree_stop_fence_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
            )?;
        if terminal_is_tree_stop_fenced {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
                &transaction,
                session_id,
                resolver_terminal_event_id,
                now,
            )?;
        }
        if !terminal_is_tree_stop_fenced
            && (terminal_resolves_owner_resume || terminal_recovers_pending_crash)
            && matches!(
                terminal.outcome,
                TurnTerminalOutcome::Completed | TurnTerminalOutcome::Failed { .. }
            )
        {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            supersede_pending_deferred_completion_in_transaction(
                &transaction,
                session_id,
                resolver_terminal_event_id,
                now,
            )?;
        }
        if !terminal_is_tree_stop_fenced
            && (terminal_resolves_owner_resume || terminal_recovers_pending_crash)
            && matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. })
        {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            discard_pending_crash_deferred_completion_in_transaction(
                &transaction,
                session_id,
                resolver_terminal_event_id,
                now,
            )?;
        }
        let recovery_has_pending_owner_resume = if orphan_recovery {
            repend_claimed_owner_resume_requests_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                now,
            )?;
            has_pending_owner_resume_requests_in_connection(&transaction, session_id)?
        } else {
            resolve_claimed_owner_resume_requests_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                now,
            )?;
            false
        };
        if terminal_is_tree_stop_fenced {
            // Only the first fence's compatible destructive terminal can reach this branch.
            // It closes the old generation without publishing a result, deferring ownership,
            // or recreating an OwnerResume.
        } else if let (Some(parent_session_id), Some(kind)) = (retained_parent, deferred_kind) {
            insert_deferred_agent_completion_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                parent_session_id,
                kind,
                now,
            )?;
            if let Some(resolver_terminal_event_id) = pending_direct_child_result {
                supersede_pending_deferred_completion_in_transaction(
                    &transaction,
                    session_id,
                    resolver_terminal_event_id,
                    now,
                )?;
                let root_session_id = transaction.query_row(
                    "SELECT root_session_id
                     FROM session_spawn_edges
                     WHERE child_session_id = ?1",
                    params![session_id.to_string()],
                    |row| row.get::<_, String>(0),
                )?;
                seed_owner_resumes_for_released_deferred_handoffs_in_transaction(
                    &transaction,
                    parse_session_id_text(&root_session_id, "deferred crash owner tree root")?,
                    now,
                )?;
            }
        } else if !recovery_has_pending_owner_resume {
            append_agent_completion_handoff_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                terminal,
                now,
            )?;
        }
        if terminal_is_tree_stop_fenced {
            // The durable fence already discarded only the generations at or before its
            // boundary. A late compatible terminal must not broaden that scope to newer work.
        } else if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            // An ordinary Stop/approval abort owns only this exact task. It may release an
            // ancestor that is now genuinely quiescent, but it never classifies descendants.
            release_quiescent_deferred_completions_after_interruption_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                now,
            )?;
        }
        let committed_terminal = terminal.clone();
        transaction.commit()?;
        Ok(GuardedTerminalization::Settled {
            commit: AdmittedTerminalCommit::Applied,
            turn_id: protocol_turn_id,
            terminal: committed_terminal,
        })
    }

    pub async fn record_model_response_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        protocol_turn_id: TurnId,
        response: ModelResponseWrite,
    ) -> Result<Vec<RunEvent>, StorageError> {
        let started_at_ms = SystemClock::now_ms();
        let ModelResponseWrite {
            response_id,
            assistant_text,
            assistant_protocol_sequence_no,
            tool_calls,
        } = response;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        let mut next_fallback_sequence_no = resolve_terminal_protocol_sequence_in_transaction(
            &transaction,
            session_id,
            protocol_turn_id,
            None,
        )?;
        let mut events = Vec::with_capacity(tool_calls.len().saturating_add(1));
        if let Some(text) = assistant_text.filter(|text| !text.is_empty()) {
            let sequence_no = assistant_protocol_sequence_no.unwrap_or(next_fallback_sequence_no);
            next_fallback_sequence_no =
                next_fallback_sequence_no.max(sequence_no.saturating_add(1));
            let event = RunEvent::AssistantMessageCommitted { response_id, text };
            insert_protocol_projection_if_requested(
                &transaction,
                &event,
                Some(session_id),
                protocol_turn_id,
                Some(sequence_no),
            )?;
            events.push(event);
        }
        for call in tool_calls {
            let sequence_no = call
                .protocol_sequence_no
                .unwrap_or(next_fallback_sequence_no);
            next_fallback_sequence_no =
                next_fallback_sequence_no.max(sequence_no.saturating_add(1));
            let event = RunEvent::ToolCallPending {
                tool_call_id: call.id,
                response_id,
                model_call_id: call.model_call_id,
                tool_name: call.tool_name,
                arguments_json: call.arguments_json,
            };
            let projection =
                project_protocol_run_event(&event, Some(session_id), protocol_turn_id, sequence_no)
                    .ok_or_else(|| {
                        StorageError::Message(
                            "ToolCallPending did not produce a protocol bundle".to_string(),
                        )
                    })?;
            let stored = insert_session_owned_event_bundle_in_transaction(
                &SESSION_PROTOCOL_WRITE_AUTHORITY,
                &transaction,
                &projection.runtime_event,
                projection.history_item.as_ref(),
                projection.turn_item.as_ref(),
            )?;
            let history_item = stored.history_item.ok_or_else(|| {
                StorageError::Message(
                    "ToolCallPending protocol bundle omitted its canonical history item"
                        .to_string(),
                )
            })?;
            validate_canonical_tool_call_payload(&history_item, call.id)?;
            transaction.execute(
                "INSERT INTO tool_calls
                 (id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms)
                 VALUES (?1, ?2, 'pending', NULL, ?3, NULL)",
                params![
                    call.id.to_string(),
                    history_item.id.to_string(),
                    started_at_ms,
                ],
            )?;
            events.push(event);
        }
        transaction.commit()?;
        Ok(events)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete_tool_call_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        title: &str,
        metadata_json: serde_json::Value,
        output_text: &str,
        truncated_output_path: Option<&camino::Utf8Path>,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<Option<RunEvent>, StorageError> {
        Ok(self
            .settle_tool_call_with_protocol_bundle(
                session_id,
                admission_id,
                tool_call_id,
                tool_name,
                ToolCallStatus::Completed,
                title,
                metadata_json,
                output_text,
                truncated_output_path,
                None,
                Vec::new(),
                protocol_turn_id,
                protocol_sequence_no,
                None,
            )
            .await?
            .map(|(tool_event, _)| tool_event))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete_tool_call_with_file_changes_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        title: &str,
        metadata_json: serde_json::Value,
        output_text: &str,
        truncated_output_path: Option<&camino::Utf8Path>,
        file_changes: Vec<crate::edit::ChangeSummary>,
        protocol_turn_id: TurnId,
        tool_output_sequence_no: Option<i64>,
        file_changes_sequence_no: Option<i64>,
    ) -> Result<Option<(RunEvent, RunEvent)>, StorageError> {
        Ok(self
            .settle_tool_call_with_protocol_bundle(
                session_id,
                admission_id,
                tool_call_id,
                tool_name,
                ToolCallStatus::Completed,
                title,
                metadata_json,
                output_text,
                truncated_output_path,
                None,
                file_changes,
                protocol_turn_id,
                tool_output_sequence_no,
                file_changes_sequence_no,
            )
            .await?
            .map(|(tool_event, file_event)| {
                (
                    tool_event,
                    file_event.expect("file-change settlement includes file event"),
                )
            }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn settle_executed_tool_call_with_file_changes_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        title: &str,
        metadata_json: serde_json::Value,
        output_text: &str,
        truncated_output_path: Option<&camino::Utf8Path>,
        status: ToolCallStatus,
        reason: &str,
        file_changes: Vec<crate::edit::ChangeSummary>,
        protocol_turn_id: TurnId,
        tool_output_sequence_no: Option<i64>,
        file_changes_sequence_no: Option<i64>,
    ) -> Result<Option<(RunEvent, RunEvent)>, StorageError> {
        if !matches!(status, ToolCallStatus::Cancelled | ToolCallStatus::Failed) {
            return Err(StorageError::Message(format!(
                "executed tool terminal settlement requires cancelled or failed status, got {}",
                status.key()
            )));
        }
        Ok(self
            .settle_tool_call_with_protocol_bundle(
                session_id,
                admission_id,
                tool_call_id,
                tool_name,
                status,
                title,
                metadata_json,
                output_text,
                truncated_output_path,
                Some(reason),
                file_changes,
                protocol_turn_id,
                tool_output_sequence_no,
                file_changes_sequence_no,
            )
            .await?
            .map(|(tool_event, file_event)| {
                (
                    tool_event,
                    file_event.expect("file-change settlement includes file event"),
                )
            }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn fail_tool_call_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        error_text: &str,
        metadata_json: serde_json::Value,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<Option<RunEvent>, StorageError> {
        Ok(self
            .settle_tool_call_with_protocol_bundle(
                session_id,
                admission_id,
                tool_call_id,
                tool_name,
                ToolCallStatus::Failed,
                "Tool failed",
                metadata_json,
                error_text,
                None,
                Some(error_text),
                Vec::new(),
                protocol_turn_id,
                protocol_sequence_no,
                None,
            )
            .await?
            .map(|(tool_event, _)| tool_event))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn settle_tool_call_without_execution_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        status: ToolCallStatus,
        reason: &str,
        metadata_json: serde_json::Value,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<Option<RunEvent>, StorageError> {
        if !matches!(status, ToolCallStatus::Declined | ToolCallStatus::Cancelled) {
            return Err(StorageError::Message(format!(
                "tool call non-execution settlement requires declined or cancelled status, got {}",
                status.key()
            )));
        }
        let title = match status {
            ToolCallStatus::Declined => "Tool declined",
            ToolCallStatus::Cancelled => "Tool cancelled",
            _ => unreachable!(),
        };
        Ok(self
            .settle_tool_call_with_protocol_bundle(
                session_id,
                admission_id,
                tool_call_id,
                tool_name,
                status,
                title,
                metadata_json,
                reason,
                None,
                None,
                Vec::new(),
                protocol_turn_id,
                protocol_sequence_no,
                None,
            )
            .await?
            .map(|(tool_event, _)| tool_event))
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_tool_call_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        status: ToolCallStatus,
        title: &str,
        metadata_json: serde_json::Value,
        output_text: &str,
        truncated_output_path: Option<&camino::Utf8Path>,
        error_text: Option<&str>,
        file_changes: Vec<crate::edit::ChangeSummary>,
        protocol_turn_id: TurnId,
        tool_output_sequence_no: Option<i64>,
        file_changes_sequence_no: Option<i64>,
    ) -> Result<Option<(RunEvent, Option<RunEvent>)>, StorageError> {
        if !matches!(
            status,
            ToolCallStatus::Completed
                | ToolCallStatus::Declined
                | ToolCallStatus::Cancelled
                | ToolCallStatus::Failed
        ) {
            return Err(StorageError::Message(format!(
                "tool settlement requires a terminal status, got {}",
                status.key()
            )));
        }
        let finished_at_ms = SystemClock::now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        validate_canonical_tool_call_in_transaction(
            &transaction,
            session_id,
            protocol_turn_id,
            tool_call_id,
            tool_name,
        )?;
        validate_persisted_file_change_ownership(&transaction, tool_call_id, &file_changes)?;
        let applied = transaction.execute(
            "UPDATE tool_calls
             SET status = ?2,
                 truncated_output_path = ?3,
                 finished_at_ms = ?4
             WHERE id = ?1
               AND history_item_id IN (
                   SELECT id FROM protocol_history_items
                   WHERE session_id = ?5 AND turn_id = ?6
               )
               AND status IN ('pending', 'running')",
            params![
                tool_call_id.to_string(),
                status.key(),
                truncated_output_path.map(|value| value.as_str()),
                finished_at_ms,
                session_id.to_string(),
                protocol_turn_id.to_string(),
            ],
        )? == 1;
        if !applied {
            transaction.commit()?;
            return Ok(None);
        }
        let tool_event = match status {
            ToolCallStatus::Completed => RunEvent::ToolCallCompleted {
                tool_call_id,
                tool: tool_name,
                title: title.to_string(),
                summary: output_text.to_string(),
                metadata: metadata_json,
            },
            ToolCallStatus::Declined => RunEvent::ToolCallDeclined {
                tool_call_id,
                tool: tool_name,
                reason: output_text.to_string(),
                metadata: metadata_json,
            },
            ToolCallStatus::Cancelled => RunEvent::ToolCallCancelled {
                tool_call_id,
                tool: tool_name,
                reason: error_text.unwrap_or(output_text).to_string(),
                metadata: metadata_json,
            },
            ToolCallStatus::Failed => RunEvent::ToolCallFailed {
                tool_call_id,
                tool: tool_name,
                error: error_text.unwrap_or(output_text).to_string(),
                metadata: metadata_json,
            },
            ToolCallStatus::Pending | ToolCallStatus::Running => unreachable!(),
        };
        insert_protocol_projection_if_requested(
            &transaction,
            &tool_event,
            Some(session_id),
            protocol_turn_id,
            tool_output_sequence_no,
        )?;
        let file_event = if file_changes.is_empty() {
            None
        } else {
            let event = RunEvent::FileChangesRecorded {
                tool_call_id,
                changes: file_changes,
            };
            insert_protocol_projection_if_requested(
                &transaction,
                &event,
                Some(session_id),
                protocol_turn_id,
                file_changes_sequence_no,
            )?;
            Some(event)
        };
        transaction.commit()?;
        Ok(Some((tool_event, file_event)))
    }
}

#[async_trait(?Send)]
impl SessionRepository for SqliteSessionRepository {
    async fn create_session(&self, draft: NewSession) -> Result<SessionRecord, StorageError> {
        let draft = normalize_new_session_draft(draft)?;
        let id = SessionId::new();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session =
            insert_session_in_transaction(&transaction, id, &draft, SystemClock.now_ms())?;
        transaction.commit()?;
        Ok(session)
    }

    async fn get_session(&self, id: SessionId) -> Result<SessionRecord, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        session_record_from_connection(&connection, id)
    }

    async fn latest_session(
        &self,
        project_id: crate::session::ProjectId,
    ) -> Result<Option<SessionRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let row = connection
            .query_row(
                "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                        model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                        status, active_run_id, active_turn_id, active_run_lease_expires_at_ms,
                        (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                         WHERE terminal_event.session_id = sessions.id
                           AND terminal_event.turn_id = sessions.active_turn_id
                           AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                        (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                         WHERE terminal_event.session_id = sessions.id
                           AND terminal_event.turn_id = sessions.active_turn_id
                           AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                         ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
                 FROM sessions
                 WHERE project_id = ?1 AND archived_at_ms IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM session_spawn_edges
                       WHERE child_session_id = sessions.id
                   )
                 ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
                 LIMIT 1",
                params![project_id.to_string()],
                session_record_with_raw_runtime_state_from_row,
            )
            .optional()?;
        let mut sessions = validate_session_record_rows(row.into_iter().collect())?;
        Ok(sessions.pop())
    }

    async fn list_sessions(
        &self,
        project_id: crate::session::ProjectId,
        limit: usize,
    ) -> Result<Vec<SessionRecord>, StorageError> {
        self.list_sessions_with_archived(project_id, limit, false)
            .await
    }

    async fn list_sessions_with_archived(
        &self,
        project_id: crate::session::ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    status, active_run_id, active_turn_id, active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
             FROM sessions
             WHERE project_id = ?1{archived_filter}
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?2"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(
                params![project_id.to_string(), sqlite_limit(limit)?],
                session_record_with_raw_runtime_state_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        validate_session_record_rows(rows)
    }

    async fn list_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    status, active_run_id, active_turn_id, active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
             FROM sessions
             WHERE archived_at_ms IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = statement
            .query_map(
                params![sqlite_limit(limit)?],
                session_record_with_raw_runtime_state_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        validate_session_record_rows(rows)
    }

    async fn search_sessions(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionRecord>, StorageError> {
        let normalized = format!(
            "%{}%",
            escape_like_literal(&query.trim().to_ascii_lowercase())
        );
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    status, active_run_id, active_turn_id, active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
             FROM sessions
             WHERE project_id = ?1{archived_filter}
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
               AND (
                   lower(title) LIKE ?2 ESCAPE '\\'
                   OR lower(cwd_path) LIKE ?2 ESCAPE '\\'
                   OR lower(model_name) LIKE ?2 ESCAPE '\\'
                   OR lower(base_url) LIKE ?2 ESCAPE '\\'
                   OR lower(access_mode) LIKE ?2 ESCAPE '\\'
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?3"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(
                params![project_id.to_string(), normalized, sqlite_limit(limit)?],
                session_record_with_raw_runtime_state_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        validate_session_record_rows(rows)
    }

    async fn set_session_archived(
        &self,
        id: SessionId,
        archived: bool,
    ) -> Result<SessionRecord, StorageError> {
        let now = SystemClock::now_ms();
        let archived_at_ms = archived.then_some(now);
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if archived
            && let Some(active_session_id) =
                active_session_for_mutation_branch(&transaction, id, true)?
        {
            return Err(StorageError::Message(format!(
                "session {id} has active or pending agent-tree session {active_session_id}; stop the agent tree before archiving it"
            )));
        }
        if archived {
            transaction.execute(
                "UPDATE sessions
                 SET archived_at_ms = ?2, updated_at_ms = ?3
                 WHERE id = ?1",
                params![id.to_string(), archived_at_ms, now],
            )?;
        } else {
            transaction.execute(
                "UPDATE sessions SET archived_at_ms = NULL, updated_at_ms = ?2 WHERE id = ?1",
                params![id.to_string(), now],
            )?;
        }
        let session = session_record_from_connection(&transaction, id)?;
        transaction.commit()?;
        Ok(session)
    }

    async fn update_session_settings(
        &self,
        id: SessionId,
        patch: &SessionSettingsPatch,
    ) -> Result<SessionSettingsUpdate, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = session_record_from_connection(&transaction, id)?;
        let next_cwd = patch.cwd.clone().unwrap_or_else(|| current.cwd.clone());
        let next_model = patch.model.clone().unwrap_or_else(|| current.model.clone());
        let next_base_url = patch
            .base_url
            .clone()
            .unwrap_or_else(|| current.base_url.clone());
        let next_base_url = ProviderEndpoint::parse(&next_base_url)
            .map_err(|error| StorageError::Message(error.to_string()))?
            .as_str()
            .to_string();
        let next_access_mode = patch.access_mode.unwrap_or(current.access_mode);
        let next_model_parameters = patch.apply_to_model_parameters(&current.model_parameters);
        let changed = next_cwd != current.cwd
            || next_model != current.model
            || next_base_url != current.base_url
            || next_access_mode != current.access_mode
            || next_model_parameters != current.model_parameters;
        if !changed {
            transaction.commit()?;
            return Ok(SessionSettingsUpdate {
                session: current,
                changed: false,
            });
        }
        if let Some(active_session_id) =
            active_session_for_mutation_branch(&transaction, id, false)?
        {
            return Err(StorageError::Message(format!(
                "session {active_session_id} is active or has a pending agent trigger; settings update requires a quiescent session"
            )));
        }
        let now = SystemClock::now_ms().max(current.updated_at_ms.saturating_add(1));
        transaction.execute(
            "UPDATE sessions
             SET cwd_path = ?2, model_name = ?3, base_url = ?4, access_mode = ?5,
                 model_parameters_json = ?6, updated_at_ms = ?7
             WHERE id = ?1",
            params![
                id.to_string(),
                next_cwd.as_str(),
                next_model,
                next_base_url,
                next_access_mode.as_str(),
                serde_json::to_string(&next_model_parameters)?,
                now,
            ],
        )?;
        let session = session_record_from_connection(&transaction, id)?;
        transaction.commit()?;
        Ok(SessionSettingsUpdate {
            session,
            changed: true,
        })
    }

    async fn update_session_title(
        &self,
        id: SessionId,
        title: &str,
    ) -> Result<SessionTitleUpdate, StorageError> {
        let current = self.get_session(id).await?;
        if current.title == title {
            return Ok(SessionTitleUpdate {
                session: current,
                changed: false,
            });
        }
        let now = SystemClock::now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE sessions SET title = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), title, now],
        )?;
        drop(connection);
        Ok(SessionTitleUpdate {
            session: self.get_session(id).await?,
            changed: true,
        })
    }

    async fn delete_session(&self, id: SessionId) -> Result<(), StorageError> {
        self.delete_session_tree(id).await?;
        Ok(())
    }
}

pub(crate) fn mutation_blocker_for_project_in_connection(
    connection: &Connection,
    project_id: ProjectId,
) -> Result<Option<SessionId>, StorageError> {
    let pending_trigger = first_unclaimed_agent_trigger_for_mutation(
        connection,
        MutationTriggerScope::Project(project_id),
    )?;
    let pending_continuation = connection
        .query_row(
            "SELECT session.id
             FROM sessions AS session
             WHERE session.project_id = ?1
               AND (
                   EXISTS (
                       SELECT 1
                       FROM agent_owner_resume_requests AS resume
                       WHERE resume.owner_session_id = session.id
                         AND resume.state IN ('pending', 'claimed')
                   )
                   OR EXISTS (
                       SELECT 1
                       FROM effective_agent_deferred_completions AS deferred
                       WHERE deferred.agent_session_id = session.id
                         AND deferred.state = 'pending'
                   )
               )
             ORDER BY session.updated_at_ms DESC, session.id DESC
             LIMIT 1",
            params![project_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|session_id| {
            parse_session_id_text(&session_id, "pending project continuation blocker")
        })
        .transpose()?;
    let mut statement = connection.prepare(
        "SELECT id, status, active_run_id, active_turn_id,
                active_run_lease_expires_at_ms,
                (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                 WHERE terminal_event.session_id = sessions.id
                   AND terminal_event.turn_id = sessions.active_turn_id
                   AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                 WHERE terminal_event.session_id = sessions.id
                   AND terminal_event.turn_id = sessions.active_turn_id
                   AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                 ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
         FROM sessions
         WHERE project_id = ?1
           AND (
               status = 'running'
               OR status NOT IN ('idle', 'completed', 'cancelled', 'failed')
               OR active_run_id IS NOT NULL
               OR active_turn_id IS NOT NULL
               OR active_run_lease_expires_at_ms IS NOT NULL
           )
         ORDER BY updated_at_ms DESC, id DESC",
    )?;
    let mut rows = statement.query(params![project_id.to_string()])?;
    let mut first_blocker = pending_trigger.or(pending_continuation);
    while let Some(row) = rows.next()? {
        let session_id = parse_session_id_column(row, 0)?;
        let runtime_state = validate_raw_session_runtime_state(
            session_id,
            raw_session_runtime_state_from_row(row, 1)?,
        )?;
        let admission_is_tree_stop_fenced = runtime_state_admission_started_before_tree_stop_fence(
            connection,
            session_id,
            runtime_state,
        )?;
        if first_blocker.is_none()
            && runtime_state.blocks_tree_mutation()
            && !admission_is_tree_stop_fenced
        {
            first_blocker = Some(session_id);
        }
    }
    Ok(first_blocker)
}

#[derive(Debug, Clone, Copy)]
enum MutationTriggerScope {
    Session(SessionId),
    Tree(SessionId),
    Project(ProjectId),
}

fn first_unclaimed_agent_trigger_for_mutation(
    connection: &Connection,
    scope: MutationTriggerScope,
) -> Result<Option<SessionId>, StorageError> {
    let (scope_kind, scope_id) = match scope {
        MutationTriggerScope::Session(session_id) => ("session", session_id.to_string()),
        MutationTriggerScope::Tree(root_session_id) => ("tree", root_session_id.to_string()),
        MutationTriggerScope::Project(project_id) => ("project", project_id.to_string()),
    };
    let session_id = connection
        .query_row(
            "SELECT mailbox.recipient_session_id
             FROM agent_mailbox_messages AS mailbox
             INNER JOIN protocol_item_append_order AS trigger_order
               ON trigger_order.session_id = mailbox.recipient_session_id
              AND trigger_order.source_kind = 'mailbox_message'
              AND trigger_order.source_id = mailbox.id
             INNER JOIN sessions AS recipient
               ON recipient.id = mailbox.recipient_session_id
             WHERE mailbox.state = 'pending'
               AND mailbox.trigger_turn = 1
               AND (
                   (?1 = 'session' AND mailbox.recipient_session_id = ?2)
                   OR (?1 = 'tree' AND mailbox.root_session_id = ?2)
                   OR (?1 = 'project' AND recipient.project_id = ?2)
               )
             ORDER BY trigger_order.append_position ASC
             LIMIT 1",
            params![scope_kind, scope_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    session_id
        .map(|session_id| parse_session_id_text(&session_id, "pending agent-tree mutation blocker"))
        .transpose()
}

fn active_session_for_mutation_branch(
    connection: &Connection,
    session_id: SessionId,
    include_descendants: bool,
) -> Result<Option<SessionId>, StorageError> {
    let canonical_root_session_id = connection.query_row(
        "SELECT COALESCE(
                 (SELECT root_session_id
                  FROM session_spawn_edges
                  WHERE child_session_id = ?1),
                 ?1
             )",
        params![session_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    let canonical_root_session_id =
        parse_session_id_text(&canonical_root_session_id, "agent-tree mutation root")?;
    let pending_trigger = first_unclaimed_agent_trigger_for_mutation(
        connection,
        if include_descendants {
            MutationTriggerScope::Tree(canonical_root_session_id)
        } else {
            MutationTriggerScope::Session(session_id)
        },
    )?;
    let pending_continuation = connection
        .query_row(
            "WITH RECURSIVE subtree(session_id) AS (
                 SELECT CASE WHEN ?2 THEN ?3 ELSE ?1 END
                 UNION
                 SELECT edge.child_session_id
                 FROM session_spawn_edges AS edge
                 INNER JOIN subtree
                   ON ?2 AND edge.parent_session_id = subtree.session_id
                 WHERE edge.root_session_id = ?3
             )
             SELECT session.id
             FROM sessions AS session
             INNER JOIN subtree ON subtree.session_id = session.id
             WHERE EXISTS (
                       SELECT 1
                       FROM agent_owner_resume_requests AS resume
                       WHERE resume.owner_session_id = session.id
                         AND resume.state IN ('pending', 'claimed')
                   )
                OR EXISTS (
                       SELECT 1
                       FROM effective_agent_deferred_completions AS deferred
                       WHERE deferred.agent_session_id = session.id
                         AND deferred.state = 'pending'
                   )
             ORDER BY session.id ASC
             LIMIT 1",
            params![
                session_id.to_string(),
                include_descendants,
                canonical_root_session_id.to_string()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|session_id| parse_session_id_text(&session_id, "pending tree continuation blocker"))
        .transpose()?;
    let mut statement = connection.prepare(
        "WITH RECURSIVE subtree(session_id) AS (
             SELECT CASE WHEN ?2 THEN ?3 ELSE ?1 END
             UNION
             SELECT edge.child_session_id
             FROM session_spawn_edges AS edge
             INNER JOIN subtree
               ON ?2 AND edge.parent_session_id = subtree.session_id
             WHERE edge.root_session_id = ?3
         )
         SELECT session.id, session.status, session.active_run_id,
                session.active_turn_id, session.active_run_lease_expires_at_ms,
                (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                 WHERE terminal_event.session_id = session.id
                   AND terminal_event.turn_id = session.active_turn_id
                   AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                 WHERE terminal_event.session_id = session.id
                   AND terminal_event.turn_id = session.active_turn_id
                   AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                 ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
         FROM sessions AS session
         INNER JOIN subtree ON subtree.session_id = session.id
         ORDER BY
             CASE
                 WHEN session.id = ?3 THEN 0
                 WHEN session.id = ?1 THEN 1
                 ELSE 2
             END,
             session.id ASC",
    )?;
    let mut rows = statement.query(params![
        session_id.to_string(),
        include_descendants,
        canonical_root_session_id.to_string()
    ])?;
    let mut first_blocker = pending_trigger.or(pending_continuation);
    while let Some(row) = rows.next()? {
        let candidate_session_id = parse_session_id_column(row, 0)?;
        let runtime_state = validate_raw_session_runtime_state(
            candidate_session_id,
            raw_session_runtime_state_from_row(row, 1)?,
        )?;
        let admission_is_tree_stop_fenced = runtime_state_admission_started_before_tree_stop_fence(
            connection,
            candidate_session_id,
            runtime_state,
        )?;
        if first_blocker.is_none()
            && runtime_state.blocks_tree_mutation()
            && !admission_is_tree_stop_fenced
        {
            first_blocker = Some(candidate_session_id);
        }
    }
    Ok(first_blocker)
}

fn parse_session_id_column(
    row: &rusqlite::Row<'_>,
    column_index: usize,
) -> rusqlite::Result<SessionId> {
    row.get::<_, String>(column_index)?
        .parse::<SessionId>()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                column_index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn session_record_with_identity_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id: parse_session_id_column(row, 0)?,
        project_id: row
            .get::<_, String>(1)?
            .parse::<ProjectId>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        title: row.get(2)?,
        status: parse_status_column(row, 3)?,
        cwd: row.get::<_, String>(4)?.into(),
        model: row.get(5)?,
        base_url: parse_provider_endpoint_column(row, 6)?,
        access_mode: parse_access_mode_column(row, 7)?,
        model_parameters: parse_session_model_parameters(&row.get::<_, String>(8)?, 8)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
        completed_at_ms: row.get(11)?,
    })
}

fn session_record_with_raw_runtime_state_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(SessionRecord, RawSessionRuntimeState)> {
    let session = session_record_with_identity_from_row(row)?;
    let raw = raw_session_runtime_state_from_row(row, 12)?;
    Ok((session, raw))
}

fn validate_session_record_rows(
    rows: Vec<(SessionRecord, RawSessionRuntimeState)>,
) -> Result<Vec<SessionRecord>, StorageError> {
    rows.into_iter()
        .map(|(session, raw)| {
            validate_raw_session_runtime_state(session.id, raw)?;
            Ok(session)
        })
        .collect()
}

#[derive(Debug)]
struct RawSessionProjectionState {
    session: SessionRecord,
    archived: bool,
    active_run_id: Option<String>,
    active_turn_id: Option<String>,
    active_run_lease_expires_at_ms: Option<i64>,
    terminal_count: i64,
    terminal_json: Option<String>,
    active_turn_sequence_no: Option<i64>,
}

fn session_projection_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RawSessionProjectionState> {
    Ok(RawSessionProjectionState {
        session: session_record_with_identity_from_row(row)?,
        archived: row.get(12)?,
        active_run_id: row.get(13)?,
        active_turn_id: row.get(14)?,
        active_run_lease_expires_at_ms: row.get(15)?,
        terminal_count: row.get(16)?,
        terminal_json: row.get(17)?,
        active_turn_sequence_no: row.get(18)?,
    })
}

fn validate_session_projection_state(
    raw: RawSessionProjectionState,
) -> Result<SessionProjectionState, StorageError> {
    let runtime_state = validate_raw_session_runtime_state(
        raw.session.id,
        RawSessionRuntimeState {
            status: session_status_text(raw.session.status).to_string(),
            active_run_id: raw.active_run_id,
            active_turn_id: raw.active_turn_id,
            active_run_lease_expires_at_ms: raw.active_run_lease_expires_at_ms,
            terminal_count: raw.terminal_count,
            terminal_json: raw.terminal_json,
        },
    )?;
    let (active_turn_id, active_turn_sequence_no) =
        if runtime_state.status == SessionStatus::Running {
            let active_turn_id = runtime_state
                .admission
                .expect("running session projection admission validated before projection")
                .turn_id;
            (
                Some(active_turn_id),
                Some(raw.active_turn_sequence_no.unwrap_or(0)),
            )
        } else {
            (None, None)
        };
    Ok(SessionProjectionState {
        session: raw.session,
        archived: raw.archived,
        active_turn_id,
        active_turn_sequence_no,
    })
}

fn sqlite_limit(limit: usize) -> Result<i64, StorageError> {
    validate_session_page_limit(limit).map_err(StorageError::Message)?;
    Ok(limit as i64)
}

fn normalize_new_session_draft(mut draft: NewSession) -> Result<NewSession, StorageError> {
    draft.base_url = ProviderEndpoint::parse(&draft.base_url)
        .map_err(|error| StorageError::Message(error.to_string()))?
        .as_str()
        .to_string();
    Ok(draft)
}

fn insert_session_in_transaction(
    transaction: &Transaction<'_>,
    id: SessionId,
    draft: &NewSession,
    now_ms: i64,
) -> Result<SessionRecord, StorageError> {
    transaction.execute(
        "INSERT INTO sessions (id, project_id, title, status, cwd_path, model_name, base_url, access_mode, model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
         VALUES (?1, ?2, ?3, 'idle', ?4, ?5, ?6, ?7, '{}', ?8, ?8, NULL)",
        params![
            id.to_string(),
            draft.project_id.to_string(),
            draft.title.as_str(),
            draft.cwd.as_str(),
            draft.model.as_str(),
            draft.base_url.as_str(),
            draft.access_mode.as_str(),
            now_ms,
        ],
    )?;
    session_record_from_connection(transaction, id)
}

fn validate_agent_child_session_draft(
    root_session: &SessionRecord,
    caller_session: &SessionRecord,
    child_draft: &NewSession,
    agent_path: &str,
    task_name: &str,
    initial_task: &InterAgentCommunication,
) -> Result<(), StorageError> {
    if root_session.project_id != caller_session.project_id {
        return Err(StorageError::Message(format!(
            "agent tree root {} and caller {} must belong to one project",
            root_session.id, caller_session.id
        )));
    }
    if child_draft.project_id != root_session.project_id {
        return Err(StorageError::Message(format!(
            "child session project {} does not match agent tree project {}",
            child_draft.project_id, root_session.project_id
        )));
    }
    // The caller's immutable turn config and Workspace materialize the child draft. Persisted
    // parent settings can legitimately predate a later root turn, so storage validates the
    // normalized draft and project authority instead of treating those older rows as a second
    // effective-config owner.
    if child_draft.title != task_name {
        return Err(StorageError::Message(format!(
            "child session title `{}` must equal its durable task name `{task_name}`",
            child_draft.title
        )));
    }
    if child_draft.model.trim().is_empty() {
        return Err(StorageError::Message(
            "child session model must not be empty".to_string(),
        ));
    }
    let child_path = AgentPath::try_from(agent_path).map_err(StorageError::Message)?;
    let expected_author = child_path.parent().ok_or_else(|| {
        StorageError::Message("an agent child path must have an immediate parent".to_string())
    })?;
    if initial_task.author != expected_author.as_str() {
        return Err(StorageError::Message(format!(
            "initial agent task author `{}` does not match immediate parent `{expected_author}`",
            initial_task.author
        )));
    }
    Ok(())
}

fn session_record_from_connection(
    connection: &Connection,
    id: SessionId,
) -> Result<SessionRecord, StorageError> {
    let (
        session,
        active_run_id,
        active_turn_id,
        active_run_lease_expires_at_ms,
        terminal_count,
        terminal_json,
    ) = connection
        .query_row(
            "SELECT project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    active_run_id, active_turn_id, active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
             FROM sessions WHERE id = ?1",
            params![id.to_string()],
            |row| {
                Ok((
                    SessionRecord {
                        id,
                        project_id: row.get::<_, String>(0)?.parse().map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        title: row.get(1)?,
                        status: parse_status_column(row, 2)?,
                        cwd: row.get::<_, String>(3)?.into(),
                        model: row.get(4)?,
                        base_url: parse_provider_endpoint_column(row, 5)?,
                        access_mode: parse_access_mode_column(row, 6)?,
                        model_parameters: parse_session_model_parameters(
                            &row.get::<_, String>(7)?,
                            7,
                        )?,
                        created_at_ms: row.get(8)?,
                        updated_at_ms: row.get(9)?,
                        completed_at_ms: row.get(10)?,
                    },
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            },
        )
        .map_err(StorageError::from)?;
    validate_raw_session_runtime_state(
        id,
        RawSessionRuntimeState {
            status: session_status_text(session.status).to_string(),
            active_run_id,
            active_turn_id,
            active_run_lease_expires_at_ms,
            terminal_count,
            terminal_json,
        },
    )?;
    Ok(session)
}

fn parse_provider_endpoint_column(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<String> {
    let raw = row.get::<_, String>(index)?;
    ProviderEndpoint::parse(&raw)
        .map(|endpoint| endpoint.as_str().to_string())
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn append_interrupted_live_snapshot_marker_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    reason: &str,
) -> Result<(), StorageError> {
    let snapshot = canonical_turn_snapshot_in_transaction(transaction, session_id, turn_id)?;
    let mut sequence_no =
        resolve_terminal_protocol_sequence_in_transaction(transaction, session_id, turn_id, None)?;
    for (call_id, tool) in snapshot.unsettled_tool_calls {
        let event = RunEvent::ToolCallCancelled {
            tool_call_id: call_id,
            tool,
            reason: reason.to_string(),
            metadata: serde_json::Value::Null,
        };
        insert_protocol_projection_if_requested(
            transaction,
            &event,
            Some(session_id),
            turn_id,
            Some(sequence_no),
        )?;
        sequence_no = sequence_no.saturating_add(1);
    }
    let event = RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(crate::session::model::DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::AgentInterrupted,
            },
            final_response_id: None,
            tool_call_count: snapshot.tool_call_count,
            failed_tool_count: snapshot.failed_tool_count,
            change_count: snapshot.change_count,
            metrics: Default::default(),
        }),
    };
    let projection = project_protocol_run_event(&event, Some(session_id), turn_id, sequence_no)
        .ok_or_else(|| {
            StorageError::Message("fork terminal marker did not produce a protocol bundle".into())
        })?;
    insert_session_owned_event_bundle_in_transaction(
        &SESSION_PROTOCOL_WRITE_AUTHORITY,
        transaction,
        &projection.runtime_event,
        projection.history_item.as_ref(),
        projection.turn_item.as_ref(),
    )?;
    Ok(())
}

#[derive(Debug)]
struct CanonicalTurnSnapshot {
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    unsettled_tool_calls: Vec<(ToolCallId, crate::tool::ToolName)>,
}

fn canonical_turn_snapshot_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<CanonicalTurnSnapshot, StorageError> {
    let payloads = {
        let mut statement = transaction.prepare(
            "SELECT payload_json
             FROM protocol_history_items
             WHERE session_id = ?1 AND turn_id = ?2
               AND json_extract(payload_json, '$.kind') IN (
                   'assistant_message', 'tool_call', 'tool_output', 'file_change'
               )
             ORDER BY sequence_no ASC, id ASC",
        )?;
        statement
            .query_map(
                params![session_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut tool_calls = Vec::<(ToolCallId, crate::tool::ToolName)>::new();
    let mut settled_tool_calls = HashSet::<ToolCallId>::new();
    let mut failed_tool_count = 0usize;
    let mut change_count = 0usize;
    for payload_json in payloads {
        match serde_json::from_str::<HistoryItemPayload>(&payload_json)? {
            HistoryItemPayload::AssistantMessage { .. } => {}
            HistoryItemPayload::ToolCall {
                call_id, tool_name, ..
            } => {
                tool_calls.push((call_id, crate::tool::ToolName::parse(&tool_name)));
            }
            HistoryItemPayload::ToolOutput {
                call_id, status, ..
            } => {
                settled_tool_calls.insert(call_id);
                if status == crate::protocol::ToolLifecycleStatus::Failed {
                    failed_tool_count = failed_tool_count.saturating_add(1);
                }
            }
            HistoryItemPayload::FileChange {
                change_ids,
                changes,
                ..
            } => {
                change_count = change_count.saturating_add(change_ids.len().max(changes.len()));
            }
            _ => {}
        }
    }
    let tool_call_count = tool_calls.len();
    let unsettled_tool_calls = tool_calls
        .into_iter()
        .filter(|(call_id, _)| !settled_tool_calls.contains(call_id))
        .collect();
    Ok(CanonicalTurnSnapshot {
        tool_call_count,
        failed_tool_count,
        change_count,
        unsettled_tool_calls,
    })
}

fn session_spawn_edge_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSpawnEdge> {
    let raw_spawn_order = row.get::<_, i64>(5)?;
    if raw_spawn_order <= 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(5, raw_spawn_order));
    }
    let edge = SessionSpawnEdge {
        root_session_id: parse_session_id_column(row, 0)?,
        parent_session_id: parse_session_id_column(row, 1)?,
        child_session_id: parse_session_id_column(row, 2)?,
        agent_path: row.get(3)?,
        task_name: row.get(4)?,
        spawn_order: raw_spawn_order
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, raw_spawn_order))?,
        created_at_ms: row.get(6)?,
    };
    validate_session_spawn_edge_shape(
        edge.root_session_id,
        edge.parent_session_id,
        edge.child_session_id,
        &edge.agent_path,
        &edge.task_name,
    )
    .map_err(|message| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
        )
    })?;
    Ok(edge)
}

fn insert_agent_mailbox_message_in_transaction(
    transaction: &Transaction<'_>,
    root_session_id: SessionId,
    author_session_id: SessionId,
    recipient_session_id: SessionId,
    communication: InterAgentCommunication,
    now_ms: i64,
    capacity_bounded: bool,
) -> Result<HistoryItemId, StorageError> {
    let pending_count = transaction.query_row(
        "SELECT COUNT(*)
         FROM agent_mailbox_messages
         WHERE recipient_session_id = ?1 AND state = 'pending'",
        params![recipient_session_id.to_string()],
        |row| row.get::<_, i64>(0),
    )?;
    if capacity_bounded
        && pending_count >= i64::try_from(MAX_DURABLE_AGENT_MAILBOX_MESSAGES).unwrap_or(i64::MAX)
    {
        return Err(StorageError::AgentMailboxFull {
            session_id: recipient_session_id,
            capacity: MAX_DURABLE_AGENT_MAILBOX_MESSAGES,
        });
    }
    let message_id = HistoryItemId::new();
    let trigger_turn = communication.trigger_turn;
    let payload_json =
        serde_json::to_string(&HistoryItemPayload::InterAgentCommunication { communication })?;
    let payload_sha256 = sha256_payload(&payload_json);
    transaction.execute(
        "INSERT INTO agent_mailbox_messages (
             id,
             root_session_id,
             author_session_id,
             recipient_session_id,
             payload_json,
             payload_sha256,
             trigger_turn,
             state,
             delivered_turn_id,
             delivered_history_item_id,
             resolved_by_terminal_event_id,
             discarded_by_stopped_session_id,
             discarded_after_append_position,
             created_at_ms,
             updated_at_ms,
             resolved_at_ms
         )
         VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending',
             NULL, NULL, NULL, NULL, NULL, ?8, ?8, NULL
         )",
        params![
            message_id.to_string(),
            root_session_id.to_string(),
            author_session_id.to_string(),
            recipient_session_id.to_string(),
            payload_json,
            payload_sha256,
            trigger_turn,
            now_ms,
        ],
    )?;
    insert_mailbox_append_order_in_transaction(
        &SESSION_PROTOCOL_WRITE_AUTHORITY,
        transaction,
        recipient_session_id,
        message_id,
        now_ms,
    )?;
    Ok(message_id)
}

fn sha256_payload(payload_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload_json.as_bytes());
    format!("{:x}", hasher.finalize())
}

struct ParsedTurnSteerPayload {
    content: Vec<ContentPart>,
    additional_context: BTreeMap<String, AdditionalContextEntry>,
    client_user_message_id: Option<String>,
}

fn parse_durable_turn_steer_payload(
    input_id: HistoryItemId,
    expected_turn_id: TurnId,
    payload_json: &str,
    payload_sha256: &str,
) -> Result<ParsedTurnSteerPayload, StorageError> {
    if sha256_payload(payload_json) != payload_sha256 {
        return Err(StorageError::Message(format!(
            "queued turn steer {input_id} payload hash does not match its durable bytes"
        )));
    }
    let payload = serde_json::from_str::<HistoryItemPayload>(payload_json)?;
    let HistoryItemPayload::SteerTurn {
        expected_turn_id: payload_turn_id,
        content,
        additional_context,
        client_user_message_id,
    } = payload
    else {
        return Err(StorageError::Message(format!(
            "queued turn steer {input_id} does not contain SteerTurn payload"
        )));
    };
    if payload_turn_id != expected_turn_id {
        return Err(StorageError::Message(format!(
            "queued turn steer {input_id} targets {payload_turn_id}, not turn {expected_turn_id}"
        )));
    }
    Ok(ParsedTurnSteerPayload {
        content,
        additional_context,
        client_user_message_id,
    })
}

fn pending_turn_input_projections_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    runtime_state: ValidatedSessionRuntimeState,
) -> Result<Vec<crate::session::PendingTurnInputProjection>, StorageError> {
    let Some(admission) = runtime_state
        .admission
        .filter(|_| runtime_state.status == SessionStatus::Running)
    else {
        return Ok(Vec::new());
    };
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT input.id,
                    input.turn_id,
                    input.payload_json,
                    input.payload_sha256,
                    input.accepted_at_ms
             FROM turn_steer_inputs AS input
             INNER JOIN turn_steer_input_enqueue_order AS enqueue
               ON enqueue.input_id = input.id
              AND enqueue.session_id = input.session_id
              AND enqueue.turn_id = input.turn_id
             WHERE input.session_id = ?1
               AND input.admission_id = ?2
               AND input.turn_id = ?3
               AND input.origin_kind = 'runtime'
               AND input.state = 'queued'
             ORDER BY enqueue.enqueue_position ASC",
        )?;
        statement
            .query_map(
                params![
                    session_id.to_string(),
                    admission.admission_id.to_string(),
                    admission.turn_id.to_string(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    rows.into_iter()
        .map(
            |(input_id, turn_id, payload_json, payload_sha256, accepted_at_ms)| {
                let input_id = input_id.parse::<HistoryItemId>().map_err(|error| {
                    StorageError::Message(format!(
                        "pending turn input has invalid identity `{input_id}`: {error}"
                    ))
                })?;
                let turn_id = turn_id.parse::<TurnId>().map_err(|error| {
                    StorageError::Message(format!(
                        "pending turn input {input_id} has invalid turn `{turn_id}`: {error}"
                    ))
                })?;
                if turn_id != admission.turn_id {
                    return Err(StorageError::Message(format!(
                        "pending turn input {input_id} belongs to {turn_id}, not active turn {}",
                        admission.turn_id
                    )));
                }
                let parsed = parse_durable_turn_steer_payload(
                    input_id,
                    turn_id,
                    &payload_json,
                    &payload_sha256,
                )?;
                let image_count = parsed
                    .content
                    .iter()
                    .filter(|part| matches!(part, ContentPart::Image { .. }))
                    .count();
                Ok(crate::session::PendingTurnInputProjection {
                    id: input_id,
                    turn_id,
                    text: content_parts_text(&parsed.content),
                    image_count,
                    accepted_at_ms,
                    client_user_message_id: parsed.client_user_message_id,
                })
            },
        )
        .collect()
}

fn count_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
) -> Result<usize, StorageError> {
    let count = transaction.query_row(
        "SELECT COUNT(*)
         FROM turn_steer_inputs
         WHERE session_id = ?1
           AND admission_id = ?2
           AND turn_id = ?3
           AND origin_kind = 'runtime'
           AND state = 'queued'",
        params![
            session_id.to_string(),
            admission_id.to_string(),
            turn_id.to_string(),
        ],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).map_err(|_| {
        StorageError::Message(format!(
            "negative pending turn-steer count for session {session_id} turn {turn_id}"
        ))
    })
}

fn deliver_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    limit: usize,
    now_ms: i64,
) -> Result<DeliveredTurnSteerPage, StorageError> {
    let query_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(129);
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT input.id, input.payload_json, input.payload_sha256
             FROM turn_steer_inputs AS input
             INNER JOIN turn_steer_input_enqueue_order AS enqueue
               ON enqueue.input_id = input.id
              AND enqueue.session_id = input.session_id
              AND enqueue.turn_id = input.turn_id
             WHERE input.session_id = ?1
               AND input.admission_id = ?2
               AND input.turn_id = ?3
               AND input.origin_kind = 'runtime'
               AND input.state = 'queued'
             ORDER BY enqueue.enqueue_position ASC
             LIMIT ?4",
        )?;
        statement
            .query_map(
                params![
                    session_id.to_string(),
                    admission_id.to_string(),
                    turn_id.to_string(),
                    query_limit,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    let has_more = rows.len() > limit;
    let mut history_item_ids = Vec::with_capacity(rows.len().min(limit));
    for (input_id, payload_json, payload_sha256) in rows.into_iter().take(limit) {
        let input_id = input_id.parse::<HistoryItemId>().map_err(|error| {
            StorageError::Message(format!(
                "queued turn steer has invalid identity `{input_id}`: {error}"
            ))
        })?;
        let ParsedTurnSteerPayload {
            content,
            additional_context,
            client_user_message_id,
        } = parse_durable_turn_steer_payload(input_id, turn_id, &payload_json, &payload_sha256)?;
        let history_item = HistoryItem {
            id: input_id,
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 0,
            created_at_ms: now_ms,
            payload: HistoryItemPayload::SteerTurn {
                expected_turn_id: turn_id,
                content: content.clone(),
                additional_context,
                client_user_message_id: client_user_message_id.clone(),
            },
        };
        let turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: Some(input_id),
            sequence_no: 0,
            payload: TurnItemPayload::SteerMessage {
                text: content_parts_text(&content),
            },
        };
        let event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: now_ms,
            msg: RuntimeEventMsg::SteerInputAccepted {
                item_count: content.len(),
                client_user_message_id,
            },
        };
        let stored = insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            transaction,
            &event,
            Some(&history_item),
            Some(&turn_item),
        )?;
        if stored.history_item.as_ref().map(|item| item.id) != Some(input_id) {
            return Err(StorageError::Message(format!(
                "turn-steer delivery for {input_id} did not preserve canonical history identity"
            )));
        }
        let transitioned = transaction.execute(
            "UPDATE turn_steer_inputs
             SET state = 'delivered',
                 delivered_history_item_id = id,
                 delivered_at_ms = ?4,
                 updated_at_ms = MAX(updated_at_ms, ?4)
             WHERE id = ?1
               AND session_id = ?2
               AND admission_id = ?3
               AND state = 'queued'",
            params![
                input_id.to_string(),
                session_id.to_string(),
                admission_id.to_string(),
                now_ms,
            ],
        )?;
        if transitioned != 1 {
            return Err(StorageError::Message(format!(
                "turn steer {input_id} lost its queued delivery owner"
            )));
        }
        history_item_ids.push(input_id);
    }
    Ok(DeliveredTurnSteerPage {
        history_item_ids,
        has_more,
    })
}

fn deliver_all_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    now_ms: i64,
) -> Result<Vec<HistoryItemId>, StorageError> {
    let mut delivered = Vec::new();
    loop {
        let page = deliver_pending_turn_steers_in_transaction(
            transaction,
            session_id,
            admission_id,
            turn_id,
            128,
            now_ms,
        )?;
        delivered.extend(page.history_item_ids);
        if !page.has_more {
            return Ok(delivered);
        }
    }
}

fn discard_all_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    terminal_event_id: RuntimeEventId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    let discarded = transaction.execute(
        "UPDATE turn_steer_inputs
         SET state = 'discarded',
             resolved_by_terminal_event_id = ?4,
             discarded_at_ms = ?5,
             updated_at_ms = MAX(updated_at_ms, ?5)
         WHERE session_id = ?1
           AND admission_id = ?2
           AND turn_id = ?3
           AND origin_kind = 'runtime'
           AND state = 'queued'",
        params![
            session_id.to_string(),
            admission_id.to_string(),
            turn_id.to_string(),
            terminal_event_id.to_string(),
            now_ms,
        ],
    )?;
    Ok(discarded)
}

fn content_parts_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Image { image } => image
                .source_path
                .as_ref()
                .map(|path| format!("{path} ({} bytes)", image.byte_len))
                .unwrap_or_else(|| format!("image attachment ({} bytes)", image.byte_len)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn canonical_agent_identity_in_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<(SessionId, AgentPath), StorageError> {
    let row = connection
        .query_row(
            "SELECT edge.root_session_id, edge.agent_path
             FROM session_spawn_edges AS edge
             WHERE edge.child_session_id = ?1",
            params![session_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    match row {
        Some((root_session_id, agent_path)) => Ok((
            parse_session_id_text(&root_session_id, "agent mailbox root")?,
            agent_path.parse::<AgentPath>().map_err(|error| {
                StorageError::Message(format!(
                    "agent mailbox session {session_id} has invalid path `{agent_path}`: {error}"
                ))
            })?,
        )),
        None => {
            let exists = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                params![session_id.to_string()],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(StorageError::Message(format!(
                    "agent mailbox session {session_id} does not exist"
                )));
            }
            Ok((session_id, AgentPath::root()))
        }
    }
}

fn validate_session_spawn_edge_shape(
    root_session_id: SessionId,
    parent_session_id: SessionId,
    child_session_id: SessionId,
    agent_path: &str,
    task_name: &str,
) -> Result<(), String> {
    if child_session_id == root_session_id {
        return Err(format!(
            "root session {root_session_id} cannot also be its own child session"
        ));
    }
    if child_session_id == parent_session_id {
        return Err(format!(
            "child session {child_session_id} cannot also be its own parent session"
        ));
    }
    AgentPath::root()
        .join(task_name)
        .map_err(|error| format!("invalid task name `{task_name}`: {error}"))?;
    let path = AgentPath::try_from(agent_path)
        .map_err(|error| format!("invalid spawn edge path `{agent_path}`: {error}"))?;
    if path.is_root() || path.name() != task_name {
        return Err(format!(
            "spawn edge path `{agent_path}` does not end with task name `{task_name}`"
        ));
    }
    Ok(())
}

fn insert_session_spawn_edge_in_transaction(
    transaction: &Transaction<'_>,
    root_session_id: SessionId,
    parent_session_id: SessionId,
    child_session_id: SessionId,
    agent_path: &str,
    task_name: &str,
) -> Result<SessionSpawnEdge, StorageError> {
    let next_spawn_order = transaction.query_row(
        "SELECT COALESCE(MAX(spawn_order), 0) + 1
         FROM session_spawn_edges
         WHERE root_session_id = ?1",
        params![root_session_id.to_string()],
        |row| row.get::<_, i64>(0),
    )?;
    let spawn_order = u64::try_from(next_spawn_order).map_err(|_| {
        StorageError::Message("durable agent spawn-order sequence is exhausted".to_string())
    })?;
    let edge = SessionSpawnEdge {
        root_session_id,
        parent_session_id,
        child_session_id,
        agent_path: agent_path.to_string(),
        task_name: task_name.to_string(),
        spawn_order,
        created_at_ms: SystemClock::now_ms(),
    };
    validate_session_spawn_edge_parent(transaction, &edge)?;
    transaction.execute(
        "INSERT INTO session_spawn_edges
         (root_session_id, parent_session_id, child_session_id, agent_path, task_name,
          spawn_order, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            edge.root_session_id.to_string(),
            edge.parent_session_id.to_string(),
            edge.child_session_id.to_string(),
            edge.agent_path,
            edge.task_name,
            i64::try_from(edge.spawn_order).map_err(|_| {
                StorageError::Message(
                    "agent spawn order exceeds the SQLite INTEGER domain".to_string(),
                )
            })?,
            edge.created_at_ms,
        ],
    )?;
    Ok(edge)
}

fn validate_session_spawn_edge_parent(
    connection: &Connection,
    edge: &SessionSpawnEdge,
) -> Result<(), StorageError> {
    validate_session_spawn_edge_shape(
        edge.root_session_id,
        edge.parent_session_id,
        edge.child_session_id,
        &edge.agent_path,
        &edge.task_name,
    )
    .map_err(StorageError::Message)?;
    let projects = connection
        .query_row(
            "SELECT root.project_id, parent.project_id, child.project_id
             FROM sessions AS root
             INNER JOIN sessions AS parent ON parent.id = ?2
             INNER JOIN sessions AS child ON child.id = ?3
             WHERE root.id = ?1",
            params![
                edge.root_session_id.to_string(),
                edge.parent_session_id.to_string(),
                edge.child_session_id.to_string()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(
                "spawn edge root, parent, and child sessions must all exist".to_string(),
            )
        })?;
    if projects.0 != projects.1 || projects.0 != projects.2 {
        return Err(StorageError::Message(format!(
            "spawn edge root {}, parent {}, and child {} must belong to one project",
            edge.root_session_id, edge.parent_session_id, edge.child_session_id
        )));
    }
    let root_is_owned = connection
        .query_row(
            "SELECT 1
             FROM session_spawn_edges
             WHERE child_session_id = ?1",
            params![edge.root_session_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if root_is_owned {
        return Err(StorageError::Message(format!(
            "session {} is already a retained descendant and cannot own another agent tree",
            edge.root_session_id
        )));
    }
    let child_owns_tree = connection
        .query_row(
            "SELECT 1
             FROM session_spawn_edges
             WHERE root_session_id = ?1
             LIMIT 1",
            params![edge.child_session_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if child_owns_tree {
        return Err(StorageError::Message(format!(
            "session {} already owns an agent tree and cannot become a retained descendant",
            edge.child_session_id
        )));
    }
    let parent_path = if edge.parent_session_id == edge.root_session_id {
        AgentPath::root()
    } else {
        let parent_path = connection
            .query_row(
                "SELECT agent_path
                 FROM session_spawn_edges
                 WHERE root_session_id = ?1 AND child_session_id = ?2",
                params![
                    edge.root_session_id.to_string(),
                    edge.parent_session_id.to_string()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "spawn parent session {} is not a retained agent in root tree {}",
                    edge.parent_session_id, edge.root_session_id
                ))
            })?;
        AgentPath::try_from(parent_path.as_str()).map_err(|error| {
            StorageError::Message(format!(
                "spawn parent session {} has invalid canonical path `{parent_path}`: {error}",
                edge.parent_session_id
            ))
        })?
    };
    let expected_path = parent_path
        .join(&edge.task_name)
        .map_err(StorageError::Message)?;
    if expected_path.as_str() != edge.agent_path {
        return Err(StorageError::Message(format!(
            "spawn edge path `{}` does not match canonical parent/task path `{expected_path}`",
            edge.agent_path
        )));
    }
    Ok(())
}

fn validate_session_spawn_edge_tree(edges: &[SessionSpawnEdge]) -> Result<(), StorageError> {
    let paths = edges
        .iter()
        .map(|edge| {
            validate_session_spawn_edge_shape(
                edge.root_session_id,
                edge.parent_session_id,
                edge.child_session_id,
                &edge.agent_path,
                &edge.task_name,
            )
            .map_err(StorageError::Message)?;
            AgentPath::try_from(edge.agent_path.as_str())
                .map(|path| ((edge.root_session_id, edge.child_session_id), path))
                .map_err(StorageError::Message)
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    for edge in edges {
        let parent_path = if edge.parent_session_id == edge.root_session_id {
            AgentPath::root()
        } else {
            paths
                .get(&(edge.root_session_id, edge.parent_session_id))
                .cloned()
                .ok_or_else(|| {
                    StorageError::Message(format!(
                        "spawn parent session {} is not retained in root tree {}",
                        edge.parent_session_id, edge.root_session_id
                    ))
                })?
        };
        let expected_path = parent_path
            .join(&edge.task_name)
            .map_err(StorageError::Message)?;
        if expected_path.as_str() != edge.agent_path {
            return Err(StorageError::Message(format!(
                "spawn edge path `{}` does not match canonical parent/task path `{expected_path}`",
                edge.agent_path
            )));
        }
    }
    Ok(())
}

fn prepare_agent_mailbox_for_session_tree_delete(
    transaction: &Transaction<'_>,
    subtree_root_session_id: SessionId,
    deleted_session_ids: &[SessionId],
) -> Result<(), StorageError> {
    let deleted_session_ids_text = deleted_session_ids
        .iter()
        .map(ToString::to_string)
        .collect::<HashSet<_>>();

    // A pending result or follow-up authored by the subtree is still owned by
    // the retained recipient. Deleting the author would either erase work the
    // owner has not observed or require an invalid pending -> tombstone
    // transition, so reject it as a stable product error before any FK/trigger
    // can surface an implementation detail.
    for author_session_id in deleted_session_ids {
        let mut statement = transaction.prepare(
            "SELECT id, recipient_session_id
             FROM agent_mailbox_messages
             WHERE author_session_id = ?1
               AND state = 'pending'
             ORDER BY created_at_ms ASC, id ASC",
        )?;
        let pending_outgoing = statement
            .query_map(params![author_session_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if let Some((mailbox_id, recipient_session_id)) =
            pending_outgoing
                .into_iter()
                .find(|(_, recipient_session_id)| {
                    !deleted_session_ids_text.contains(recipient_session_id)
                })
        {
            return Err(StorageError::Message(format!(
                "cannot delete session subtree {subtree_root_session_id}: pending owner mail exists from deleted author session {author_session_id} to retained recipient session {recipient_session_id} (mailbox {mailbox_id}); deliver or discard it before deleting the author"
            )));
        }
    }

    // Remove dependants first. A delivered/discarded message whose recipient is
    // outside the subtree remains durable; its author FK is tombstoned by
    // ON DELETE SET NULL after completion-handoff metadata is detached.
    for session_id in deleted_session_ids {
        let session_id = session_id.to_string();
        transaction.execute(
            "DELETE FROM agent_owner_resume_requests
             WHERE owner_session_id = ?1
                OR source_session_id = ?1
                OR source_history_item_id IN (
                    SELECT parent_history_item_id
                    FROM agent_completion_handoffs
                    WHERE child_session_id = ?1 OR parent_session_id = ?1
                )
                OR source_history_item_id IN (
                    SELECT id
                    FROM agent_mailbox_messages
                    WHERE recipient_session_id = ?1
                )",
            params![session_id],
        )?;
    }
    for session_id in deleted_session_ids {
        let session_id = session_id.to_string();
        transaction.execute(
            "DELETE FROM agent_completion_handoffs
             WHERE child_session_id = ?1
                OR parent_session_id = ?1
                OR parent_history_item_id IN (
                    SELECT id
                    FROM agent_mailbox_messages
                    WHERE recipient_session_id = ?1
                )",
            params![session_id],
        )?;
    }
    for session_id in deleted_session_ids {
        let session_id = session_id.to_string();
        transaction.execute(
            "DELETE FROM protocol_item_append_order
             WHERE session_id = ?1
               AND source_kind = 'mailbox_message'
               AND source_id IN (
                   SELECT id
                   FROM agent_mailbox_messages
                   WHERE recipient_session_id = ?1
               )",
            params![session_id],
        )?;
        transaction.execute(
            "DELETE FROM agent_mailbox_messages
             WHERE recipient_session_id = ?1",
            params![session_id],
        )?;
    }
    Ok(())
}

fn delete_session_rows(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<(), StorageError> {
    let session_id = session_id.to_string();
    transaction.execute(
        "DELETE FROM agent_owner_resume_requests
         WHERE owner_session_id = ?1 OR source_session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM agent_deferred_completions
         WHERE agent_session_id = ?1
            OR parent_session_id = ?1
            OR resolved_by_terminal_event_id IN (
                SELECT id
                FROM protocol_runtime_events
                WHERE session_id = ?1
            )",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM agent_completion_handoffs
         WHERE child_session_id = ?1 OR parent_session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM harness_replay_reports
         WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM harness_gate_results
         WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM harness_contracts
         WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM harness_artifacts
         WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM harness_events
         WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM harness_runs WHERE session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM turn_steer_inputs WHERE session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM protocol_turn_items WHERE session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM protocol_history_items WHERE session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM protocol_runtime_events WHERE session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM protocol_item_append_order WHERE session_id = ?1",
        params![session_id],
    )?;
    transaction.execute(
        "DELETE FROM protocol_turn_sequence_allocators WHERE session_id = ?1",
        params![session_id],
    )?;
    transaction.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
    Ok(())
}

fn validate_terminal_event(
    target_session_id: SessionId,
    event: &RunEvent,
) -> Result<&crate::session::model::DurableTurnTerminal, StorageError> {
    let RunEvent::TurnTerminal {
        session_id,
        terminal,
    } = event
    else {
        return Err(StorageError::Message(
            "terminal session mutation requires RunEvent::TurnTerminal".to_string(),
        ));
    };
    if *session_id != target_session_id {
        return Err(StorageError::Message(format!(
            "terminal event belongs to session {session_id}, not target session {target_session_id}"
        )));
    }
    if terminal.failed_tool_count > terminal.tool_call_count {
        return Err(StorageError::Message(format!(
            "TurnTerminal failed tool count {} exceeds total tool count {}",
            terminal.failed_tool_count, terminal.tool_call_count
        )));
    }
    Ok(terminal)
}

fn pending_agent_trigger_history_item_id_in_connection(
    connection: &Connection,
    session_id: SessionId,
    tree_stop_fence: Option<AgentTreeStopFence>,
) -> Result<Option<HistoryItemId>, StorageError> {
    let history_item_id = connection
        .query_row(
            "SELECT mailbox.id
             FROM agent_mailbox_messages AS mailbox
             INNER JOIN protocol_item_append_order AS enqueue_order
               ON enqueue_order.session_id = mailbox.recipient_session_id
              AND enqueue_order.source_kind = 'mailbox_message'
              AND enqueue_order.source_id = mailbox.id
             WHERE mailbox.recipient_session_id = ?1
               AND mailbox.trigger_turn = 1
               AND (
                   (?2 IS NULL AND mailbox.state = 'pending')
                   OR
                   (?2 IS NOT NULL
                    AND mailbox.state = 'discarded'
                    AND mailbox.root_session_id = ?3
                    AND mailbox.discarded_by_stopped_session_id = ?4
                    AND mailbox.discarded_after_append_position = ?2)
               )
             ORDER BY enqueue_order.append_position ASC
             LIMIT 1",
            params![
                session_id.to_string(),
                tree_stop_fence.map(|fence| fence.after_append_position),
                tree_stop_fence.map(|fence| fence.root_session_id.to_string()),
                tree_stop_fence.map(|fence| fence.stopped_session_id.to_string()),
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    history_item_id
        .map(|history_item_id| {
            history_item_id.parse::<HistoryItemId>().map_err(|error| {
                StorageError::Message(format!(
                    "pending agent trigger for session {session_id} has invalid history id `{history_item_id}`: {error}"
                ))
            })
        })
        .transpose()
}

fn agent_trigger_append_position_authorized_by_tree_stop_fence_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    expected_history_item_id: HistoryItemId,
    fence: AgentTreeStopFence,
) -> Result<Option<i64>, StorageError> {
    if !session_belongs_to_exact_tree_stop_fence_scope_in_connection(
        transaction,
        session_id,
        fence,
    )? {
        return Ok(None);
    }
    transaction
        .query_row(
            "SELECT trigger_order.append_position
             FROM agent_mailbox_messages AS trigger
             INNER JOIN protocol_item_append_order AS trigger_order
               ON trigger_order.session_id = trigger.recipient_session_id
              AND trigger_order.source_kind = 'mailbox_message'
              AND trigger_order.source_id = trigger.id
             WHERE trigger.recipient_session_id = ?1
               AND trigger.id = ?2
               AND trigger.trigger_turn = 1
               AND trigger.state = 'discarded'
               AND trigger.root_session_id = ?4
               AND trigger.discarded_by_stopped_session_id = ?5
               AND trigger.discarded_after_append_position = ?3
               AND trigger_order.append_position <= ?3",
            params![
                session_id.to_string(),
                expected_history_item_id.to_string(),
                fence.after_append_position,
                fence.root_session_id.to_string(),
                fence.stopped_session_id.to_string(),
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(StorageError::from)
}

fn session_belongs_to_exact_tree_stop_fence_scope_in_connection(
    connection: &Connection,
    session_id: SessionId,
    fence: AgentTreeStopFence,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "WITH RECURSIVE stopped_scope(session_id) AS (
                 SELECT stored.stopped_session_id
                 FROM agent_tree_stop_fences AS stored
                 WHERE stored.root_session_id = ?1
                   AND stored.stopped_session_id = ?2
                   AND stored.after_append_position = ?3
                 UNION ALL
                 SELECT child.child_session_id
                 FROM stopped_scope AS parent
                 INNER JOIN session_spawn_edges AS child
                   ON child.root_session_id = ?1
                  AND child.parent_session_id = parent.session_id
             )
             SELECT EXISTS (
                 SELECT 1
                 FROM stopped_scope
                 WHERE stopped_scope.session_id = ?4
             )",
            params![
                fence.root_session_id.to_string(),
                fence.stopped_session_id.to_string(),
                fence.after_append_position,
                session_id.to_string(),
            ],
            |row| row.get::<_, bool>(0),
        )
        .map_err(StorageError::from)
}

fn pending_agent_trigger_is_unclaimed_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    expected_history_item_id: HistoryItemId,
    include_tree_stop_fenced: bool,
) -> Result<bool, StorageError> {
    let stored = transaction
        .query_row(
            "SELECT mailbox.state,
                    mailbox.trigger_turn,
                    mailbox.payload_json,
                    edge.agent_path,
                    mailbox.discarded_by_stopped_session_id
             FROM agent_mailbox_messages AS mailbox
             LEFT JOIN session_spawn_edges AS edge
               ON edge.child_session_id = mailbox.recipient_session_id
             WHERE mailbox.recipient_session_id = ?1
               AND mailbox.id = ?2",
            params![session_id.to_string(), expected_history_item_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((state, trigger_turn, payload_json, agent_path, discarded_by_stop)) = stored else {
        return Err(StorageError::Message(format!(
            "agent trigger mailbox message {expected_history_item_id} does not belong to session {session_id}"
        )));
    };
    if !trigger_turn {
        return Err(StorageError::Message(format!(
            "agent trigger mailbox message {expected_history_item_id} does not request a turn"
        )));
    }
    let agent_path = agent_path.ok_or_else(|| {
        StorageError::Message(format!(
            "agent trigger session {session_id} is not a retained descendant"
        ))
    })?;
    let payload = serde_json::from_str::<HistoryItemPayload>(&payload_json)?;
    let HistoryItemPayload::InterAgentCommunication { communication } = payload else {
        return Err(StorageError::Message(format!(
            "agent trigger mailbox message {expected_history_item_id} is not inter-agent communication"
        )));
    };
    if communication.recipient != agent_path {
        return Err(StorageError::Message(format!(
            "agent trigger mailbox message {expected_history_item_id} targets `{}` instead of canonical child `{agent_path}`",
            communication.recipient
        )));
    }
    Ok(match state.as_str() {
        "pending" => true,
        "discarded" => include_tree_stop_fenced && discarded_by_stop.is_some(),
        "delivered" => false,
        other => {
            return Err(StorageError::Message(format!(
                "agent trigger mailbox message {expected_history_item_id} has unknown state `{other}`"
            )));
        }
    })
}

fn insert_agent_trigger_turn_claim_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    history_item_id: HistoryItemId,
    now_ms: i64,
) -> Result<(), StorageError> {
    let inserted = transaction.execute(
        "INSERT INTO agent_trigger_turn_claims (
             history_item_id,
             recipient_session_id,
             admission_id,
             turn_id,
             created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            history_item_id.to_string(),
            session_id.to_string(),
            admission_id.to_string(),
            turn_id.to_string(),
            now_ms,
        ],
    )?;
    if inserted != 1 {
        return Err(StorageError::Message(format!(
            "explicit agent wake {history_item_id} did not acquire its exact turn claim"
        )));
    }
    Ok(())
}

fn agent_trigger_turn_claim_in_connection(
    connection: &Connection,
    session_id: SessionId,
    history_item_id: HistoryItemId,
) -> Result<Option<(AdmissionId, TurnId)>, StorageError> {
    let claim = connection
        .query_row(
            "SELECT admission_id, turn_id
             FROM agent_trigger_turn_claims
             WHERE recipient_session_id = ?1
               AND history_item_id = ?2",
            params![session_id.to_string(), history_item_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    claim
        .map(|(admission_id, turn_id)| {
            let admission_id = admission_id.parse::<AdmissionId>().map_err(|error| {
                StorageError::Message(format!(
                    "explicit agent wake {history_item_id} has invalid admission id `{admission_id}`: {error}"
                ))
            })?;
            let turn_id = turn_id.parse::<TurnId>().map_err(|error| {
                StorageError::Message(format!(
                    "explicit agent wake {history_item_id} has invalid turn id `{turn_id}`: {error}"
                ))
            })?;
            Ok((admission_id, turn_id))
        })
        .transpose()
}

fn agent_trigger_history_item_for_turn_in_connection(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<HistoryItemId>, StorageError> {
    let history_item_id = connection
        .query_row(
            "SELECT history_item_id
             FROM agent_trigger_turn_claims
             WHERE recipient_session_id = ?1
               AND turn_id = ?2",
            params![session_id.to_string(), turn_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    history_item_id
        .map(|history_item_id| {
            history_item_id.parse::<HistoryItemId>().map_err(|error| {
                StorageError::Message(format!(
                    "explicit agent turn {turn_id} has invalid wake id `{history_item_id}`: {error}"
                ))
            })
        })
        .transpose()
}

fn discard_pending_explicit_agent_wake_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    history_item_id: HistoryItemId,
    terminal_event_id: RuntimeEventId,
    now_ms: i64,
) -> Result<(), StorageError> {
    let state = transaction
        .query_row(
            "SELECT state
             FROM agent_mailbox_messages
             WHERE id = ?1
               AND recipient_session_id = ?2
               AND trigger_turn = 1",
            params![history_item_id.to_string(), session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "explicit agent wake {history_item_id} no longer belongs to session {session_id}"
            ))
        })?;
    if state != "pending" {
        return Ok(());
    }
    let discarded = transaction.execute(
        "UPDATE agent_mailbox_messages
         SET state = 'discarded',
             resolved_by_terminal_event_id = ?2,
             updated_at_ms = MAX(updated_at_ms, ?3),
             resolved_at_ms = ?3
         WHERE id = ?1
           AND recipient_session_id = ?4
           AND state = 'pending'",
        params![
            history_item_id.to_string(),
            terminal_event_id.to_string(),
            now_ms,
            session_id.to_string(),
        ],
    )?;
    if discarded != 1 {
        return Err(StorageError::Message(format!(
            "explicit agent wake {history_item_id} lost its exact interrupted terminal owner"
        )));
    }
    Ok(())
}

fn owner_resume_claimed_turn_in_connection(
    connection: &Connection,
    session_id: SessionId,
    request_id: OwnerResumeRequestId,
) -> Result<Option<TurnId>, StorageError> {
    let claimed_turn_id = connection
        .query_row(
            "SELECT claimed_turn_id
             FROM agent_owner_resume_requests
             WHERE owner_session_id = ?1
               AND source_history_item_id = ?2
               AND claimed_turn_id IS NOT NULL",
            params![session_id.to_string(), request_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    claimed_turn_id
        .map(|turn_id| {
            turn_id.parse::<TurnId>().map_err(|error| {
                StorageError::Message(format!(
                    "OwnerResume request {request_id} has invalid claimed turn id `{turn_id}`: {error}"
                ))
            })
        })
        .transpose()
}

fn terminal_for_turn_in_connection(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<crate::session::model::DurableTurnTerminal>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT msg_json
         FROM protocol_runtime_events
         WHERE session_id = ?1 AND turn_id = ?2
           AND json_extract(msg_json, '$.kind') = 'turn_terminal'
         ORDER BY sequence_no DESC, rowid DESC
         LIMIT 2",
    )?;
    let mut rows = statement.query_map(
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    let RuntimeEventMsg::TurnTerminal { terminal } =
        serde_json::from_str::<RuntimeEventMsg>(&row?)?
    else {
        return Err(StorageError::Message(
            "terminal runtime-event discriminator did not decode as TurnTerminal".to_string(),
        ));
    };
    if rows.next().transpose()?.is_some() {
        return Err(StorageError::Message(format!(
            "multiple durable terminals exist for session {session_id} turn {turn_id}"
        )));
    }
    Ok(Some(*terminal))
}

fn session_has_durable_descendant_work_in_connection(
    connection: &Connection,
    owner_session_id: SessionId,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "WITH RECURSIVE descendants(session_id) AS (
                 SELECT edge.child_session_id
                 FROM session_spawn_edges AS edge
                 WHERE edge.parent_session_id = ?1
                 UNION ALL
                 SELECT edge.child_session_id
                 FROM session_spawn_edges AS edge
                 INNER JOIN descendants AS parent
                   ON edge.parent_session_id = parent.session_id
             )
             SELECT EXISTS (
                 SELECT 1
                 FROM descendants
                 INNER JOIN sessions AS descendant
                   ON descendant.id = descendants.session_id
                 WHERE (
                        (
                            descendant.status = 'running'
                            OR descendant.active_run_id IS NOT NULL
                        )
                        AND NOT EXISTS (
                            WITH RECURSIVE fenced_scope(
                                root_session_id,
                                stopped_session_id,
                                after_append_position,
                                session_id
                            ) AS (
                                SELECT
                                    fence.root_session_id,
                                    fence.stopped_session_id,
                                    fence.after_append_position,
                                    fence.stopped_session_id
                                FROM agent_tree_stop_fences AS fence
                                UNION ALL
                                SELECT
                                    parent.root_session_id,
                                    parent.stopped_session_id,
                                    parent.after_append_position,
                                    edge.child_session_id
                                FROM fenced_scope AS parent
                                INNER JOIN session_spawn_edges AS edge
                                  ON edge.root_session_id =
                                     parent.root_session_id
                                 AND edge.parent_session_id =
                                     parent.session_id
                            )
                            SELECT 1
                            FROM fenced_scope AS fence
                            WHERE fence.session_id = descendant.id
                              AND (
                                  SELECT MIN(turn_order.append_position)
                                  FROM protocol_item_append_order AS turn_order
                                  WHERE turn_order.session_id = descendant.id
                                    AND turn_order.turn_id =
                                        descendant.active_turn_id
                              ) <= fence.after_append_position
                        )
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM agent_owner_resume_requests AS resume
                        WHERE resume.owner_session_id = descendant.id
                          AND resume.state IN ('pending', 'claimed')
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM effective_agent_deferred_completions AS deferred
                        WHERE deferred.agent_session_id = descendant.id
                          AND deferred.state = 'pending'
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM agent_mailbox_messages AS mailbox
                        WHERE mailbox.recipient_session_id = descendant.id
                          AND mailbox.state = 'pending'
                          AND mailbox.trigger_turn = 1
                    )
             )",
            params![owner_session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )
        .map_err(StorageError::from)
}

fn retained_agent_parent_in_connection(
    connection: &Connection,
    agent_session_id: SessionId,
) -> Result<Option<SessionId>, StorageError> {
    connection
        .query_row(
            "SELECT parent_session_id
             FROM session_spawn_edges
             WHERE child_session_id = ?1",
            params![agent_session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|parent_session_id| parse_session_id_text(&parent_session_id, "retained agent parent"))
        .transpose()
}

fn insert_deferred_agent_completion_in_transaction(
    transaction: &Transaction<'_>,
    agent_session_id: SessionId,
    agent_turn_id: TurnId,
    parent_session_id: SessionId,
    kind: DeferredAgentCompletionKind,
    now_ms: i64,
) -> Result<(), StorageError> {
    let terminal_event_id =
        exact_terminal_event_id_in_transaction(transaction, agent_session_id, agent_turn_id)?;
    transaction.execute(
        "INSERT INTO agent_deferred_completions (
             agent_session_id,
             agent_turn_id,
             terminal_event_id,
             parent_session_id,
             kind,
             state,
             resolved_by_terminal_event_id,
             created_at_ms,
             updated_at_ms,
             resolved_at_ms
         )
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, ?6, ?6, NULL)",
        params![
            agent_session_id.to_string(),
            agent_turn_id.to_string(),
            terminal_event_id.to_string(),
            parent_session_id.to_string(),
            match kind {
                DeferredAgentCompletionKind::CompletedEarly => "completed_early",
                DeferredAgentCompletionKind::CrashFailed => "crash_failed",
            },
            now_ms,
        ],
    )?;
    Ok(())
}

fn supersede_pending_deferred_completion_in_transaction(
    transaction: &Transaction<'_>,
    owner_session_id: SessionId,
    resolver_terminal_event_id: RuntimeEventId,
    now_ms: i64,
) -> Result<Option<TurnId>, StorageError> {
    let pending_turn_id = transaction
        .query_row(
            "SELECT agent_turn_id
             FROM effective_agent_deferred_completions
             WHERE agent_session_id = ?1
               AND state = 'pending'
             LIMIT 1",
            params![owner_session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(pending_turn_id) = pending_turn_id else {
        return Ok(None);
    };
    let updated = transaction.execute(
        "UPDATE agent_deferred_completions
         SET state = 'superseded',
             resolved_by_terminal_event_id = ?2,
             resolved_at_ms = ?3,
             updated_at_ms = MAX(updated_at_ms, ?3)
         WHERE agent_session_id = ?1
           AND agent_turn_id = ?4
           AND state = 'pending'",
        params![
            owner_session_id.to_string(),
            resolver_terminal_event_id.to_string(),
            now_ms,
            pending_turn_id,
        ],
    )?;
    if updated != 1 {
        return Err(StorageError::Message(format!(
            "pending deferred completion for owner {owner_session_id} lost exact generation ownership"
        )));
    }
    pending_turn_id
        .parse::<TurnId>()
        .map(Some)
        .map_err(|error| {
            StorageError::Message(format!(
                "pending deferred completion for owner {owner_session_id} has invalid turn id `{pending_turn_id}`: {error}"
            ))
        })
}

fn discard_pending_crash_deferred_completion_in_transaction(
    transaction: &Transaction<'_>,
    owner_session_id: SessionId,
    resolver_terminal_event_id: RuntimeEventId,
    now_ms: i64,
) -> Result<bool, StorageError> {
    Ok(transaction.execute(
        "UPDATE agent_deferred_completions
         SET state = 'discarded',
             resolved_by_terminal_event_id = ?2,
             resolved_at_ms = ?3,
             updated_at_ms = MAX(updated_at_ms, ?3)
         WHERE agent_session_id = ?1
           AND kind = 'crash_failed'
           AND state = 'pending'",
        params![
            owner_session_id.to_string(),
            resolver_terminal_event_id.to_string(),
            now_ms,
        ],
    )? == 1)
}

fn discard_pending_deferred_completion_for_self_stop_in_transaction(
    transaction: &Transaction<'_>,
    owner_session_id: SessionId,
    resolver_terminal_event_id: RuntimeEventId,
    now_ms: i64,
) -> Result<bool, StorageError> {
    Ok(transaction.execute(
        "UPDATE agent_deferred_completions
         SET state = 'discarded',
             resolved_by_terminal_event_id = ?2,
             resolved_at_ms = ?3,
             updated_at_ms = MAX(updated_at_ms, ?3)
         WHERE agent_session_id = ?1
           AND state = 'pending'",
        params![
            owner_session_id.to_string(),
            resolver_terminal_event_id.to_string(),
            now_ms,
        ],
    )? == 1)
}

fn pending_direct_child_result_terminal_in_connection(
    connection: &Connection,
    owner_session_id: SessionId,
) -> Result<Option<RuntimeEventId>, StorageError> {
    connection
        .query_row(
            "SELECT handoff.child_terminal_event_id
             FROM agent_completion_handoffs AS handoff
             INNER JOIN agent_mailbox_messages AS mailbox
               ON mailbox.id = handoff.parent_history_item_id
              AND mailbox.recipient_session_id = handoff.parent_session_id
              AND mailbox.state = 'pending'
             INNER JOIN protocol_item_append_order AS result_order
               ON result_order.session_id = mailbox.recipient_session_id
              AND result_order.source_kind = 'mailbox_message'
              AND result_order.source_id = mailbox.id
             WHERE handoff.parent_session_id = ?1
             ORDER BY result_order.append_position ASC
             LIMIT 1",
            params![owner_session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|terminal_event_id| {
            terminal_event_id.parse::<RuntimeEventId>().map_err(|error| {
                StorageError::Message(format!(
                    "pending direct-child result has invalid terminal event id `{terminal_event_id}`: {error}"
                ))
            })
        })
        .transpose()
}

fn record_agent_tree_stop_fence_in_transaction(
    transaction: &Transaction<'_>,
    stopped_session_id: SessionId,
    cause: &str,
    now_ms: i64,
) -> Result<Option<AgentTreeStopFence>, StorageError> {
    let root_session_id = transaction
        .query_row(
            "SELECT edge.root_session_id
             FROM session_spawn_edges AS edge
             WHERE edge.child_session_id = ?1",
            params![stopped_session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|root_session_id| parse_session_id_text(&root_session_id, "tree-stop fence root"))
        .transpose()?
        .unwrap_or(stopped_session_id);
    let stopped_exists = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = ?1)",
        params![stopped_session_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if !stopped_exists {
        return Ok(None);
    }
    let after_append_position = transaction.query_row(
        "SELECT MAX(
             COALESCE(
                 (SELECT MAX(append_position)
                  FROM protocol_item_append_order),
                 0
             ),
             COALESCE(
                 (SELECT seq
                  FROM sqlite_sequence
                  WHERE name = 'protocol_item_append_order'),
                 0
             )
         )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO agent_tree_stop_fences (
             root_session_id,
             stopped_session_id,
             after_append_position,
             cause,
             created_at_ms
         )
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            root_session_id.to_string(),
            stopped_session_id.to_string(),
            after_append_position,
            cause,
            now_ms,
        ],
    )?;
    transaction.execute(
        "WITH RECURSIVE stopped_scope(session_id) AS (
             SELECT ?1
             UNION ALL
             SELECT edge.child_session_id
             FROM session_spawn_edges AS edge
             INNER JOIN stopped_scope AS parent
               ON edge.root_session_id = ?2
              AND edge.parent_session_id = parent.session_id
         )
         UPDATE agent_mailbox_messages
         SET state = 'discarded',
             discarded_by_stopped_session_id = ?1,
             discarded_after_append_position = ?3,
             updated_at_ms = MAX(updated_at_ms, ?4),
             resolved_at_ms = ?4
         WHERE state = 'pending'
           AND root_session_id = ?2
           AND recipient_session_id IN (SELECT session_id FROM stopped_scope)
           AND EXISTS (
               SELECT 1
               FROM protocol_item_append_order AS enqueue_order
               WHERE enqueue_order.session_id =
                     agent_mailbox_messages.recipient_session_id
                 AND enqueue_order.source_kind = 'mailbox_message'
                 AND enqueue_order.source_id = agent_mailbox_messages.id
                 AND enqueue_order.append_position <= ?3
           )",
        params![
            stopped_session_id.to_string(),
            root_session_id.to_string(),
            after_append_position,
            now_ms,
        ],
    )?;
    transaction.execute(
        "WITH RECURSIVE stopped_scope(session_id) AS (
             SELECT ?1
             UNION ALL
             SELECT edge.child_session_id
             FROM session_spawn_edges AS edge
             INNER JOIN stopped_scope AS parent
               ON edge.root_session_id = ?2
              AND edge.parent_session_id = parent.session_id
         )
         UPDATE agent_owner_resume_requests
         SET state = 'cancelled',
             resolved_at_ms = ?3,
             updated_at_ms = MAX(updated_at_ms, ?3)
         WHERE state IN ('pending', 'claimed')
           AND (
               owner_session_id IN (SELECT session_id FROM stopped_scope)
               OR EXISTS (
                   SELECT 1
                   FROM agent_completion_handoffs AS handoff
                   WHERE handoff.parent_history_item_id =
                         agent_owner_resume_requests.source_history_item_id
                     AND handoff.child_session_id IN (
                         SELECT session_id FROM stopped_scope
                     )
                     AND (
                         SELECT MIN(child_turn_order.append_position)
                         FROM protocol_item_append_order AS child_turn_order
                         WHERE child_turn_order.session_id =
                               handoff.child_session_id
                           AND child_turn_order.turn_id =
                               handoff.child_turn_id
                     ) <= ?4
               )
           )",
        params![
            stopped_session_id.to_string(),
            root_session_id.to_string(),
            now_ms,
            after_append_position,
        ],
    )?;
    transaction.execute(
        "WITH RECURSIVE
         stopped_scope(session_id) AS (
             SELECT ?1
             UNION ALL
             SELECT edge.child_session_id
             FROM session_spawn_edges AS edge
             INNER JOIN stopped_scope AS parent
               ON edge.root_session_id = ?2
              AND edge.parent_session_id = parent.session_id
         ),
         stopped_ancestors(session_id) AS (
             SELECT ?1
             UNION ALL
             SELECT edge.parent_session_id
             FROM session_spawn_edges AS edge
             INNER JOIN stopped_ancestors AS child
               ON edge.root_session_id = ?2
              AND edge.child_session_id = child.session_id
         )
         UPDATE agent_deferred_completions
         SET state = 'discarded',
             resolved_by_terminal_event_id = terminal_event_id,
             resolved_at_ms = ?3,
             updated_at_ms = MAX(updated_at_ms, ?3)
         WHERE state = 'pending'
           AND agent_session_id IN (
               SELECT session_id FROM stopped_scope
               UNION
               SELECT session_id FROM stopped_ancestors
           )
           AND (
               SELECT MIN(turn_order.append_position)
               FROM protocol_item_append_order AS turn_order
               WHERE turn_order.session_id =
                     agent_deferred_completions.agent_session_id
                 AND turn_order.turn_id =
                     agent_deferred_completions.agent_turn_id
           ) <= ?4
           AND EXISTS (
               SELECT 1
               FROM protocol_item_append_order AS terminal_order
               WHERE terminal_order.session_id =
                     agent_deferred_completions.agent_session_id
                 AND terminal_order.source_kind = 'runtime_event'
                 AND terminal_order.source_id =
                     agent_deferred_completions.terminal_event_id
                 AND terminal_order.append_position <= ?4
           )",
        params![
            stopped_session_id.to_string(),
            root_session_id.to_string(),
            now_ms,
            after_append_position,
        ],
    )?;
    Ok(Some(AgentTreeStopFence {
        root_session_id,
        stopped_session_id,
        after_append_position,
    }))
}

fn explicit_agent_tree_stop_fence_cause(
    cause: crate::protocol::TurnInterruptionCause,
) -> Result<&'static str, StorageError> {
    match cause {
        crate::protocol::TurnInterruptionCause::ApprovalAborted => Ok("approval_aborted"),
        crate::protocol::TurnInterruptionCause::UserStop => Ok("user_stop"),
        crate::protocol::TurnInterruptionCause::TreeStopped => Err(StorageError::Message(
            "a derived tree-stopped terminal must reuse its ancestor Stop fence".to_string(),
        )),
        crate::protocol::TurnInterruptionCause::AgentInterrupted => Err(StorageError::Message(
            "an agent-interrupted turn is reusable and cannot own a tree-stop fence".to_string(),
        )),
    }
}

fn turn_started_before_applicable_tree_stop_fence_in_transaction(
    transaction: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<bool, StorageError> {
    Ok(
        first_applicable_tree_stop_fence_for_turn_in_connection(transaction, session_id, turn_id)?
            .is_some(),
    )
}

fn first_applicable_tree_stop_fence_for_turn_in_connection(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<ApplicableAgentTreeStopFence>, StorageError> {
    let Some(turn_start_position) =
        turn_start_append_position_in_connection(connection, session_id, turn_id)?
    else {
        return Ok(None);
    };
    first_applicable_tree_stop_fence_at_append_position_in_connection(
        connection,
        session_id,
        turn_start_position,
    )
}

fn turn_start_append_position_in_connection(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<i64>, StorageError> {
    connection
        .query_row(
            "SELECT MIN(append_position)
         FROM protocol_item_append_order
         WHERE session_id = ?1 AND turn_id = ?2",
            params![session_id.to_string(), turn_id.to_string()],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(StorageError::from)
}

fn first_applicable_tree_stop_fence_at_append_position_in_connection(
    connection: &Connection,
    session_id: SessionId,
    append_position: i64,
) -> Result<Option<ApplicableAgentTreeStopFence>, StorageError> {
    let stored = connection
        .query_row(
            "WITH RECURSIVE fenced_scope(
                 fence_rowid,
                 root_session_id,
                 stopped_session_id,
                 after_append_position,
                 cause,
                 session_id
             ) AS (
                 SELECT
                     fence.rowid,
                     fence.root_session_id,
                     fence.stopped_session_id,
                     fence.after_append_position,
                     fence.cause,
                     fence.stopped_session_id
                 FROM agent_tree_stop_fences AS fence
                 UNION ALL
                 SELECT
                     parent.fence_rowid,
                     parent.root_session_id,
                     parent.stopped_session_id,
                     parent.after_append_position,
                     parent.cause,
                     edge.child_session_id
                 FROM fenced_scope AS parent
                 INNER JOIN session_spawn_edges AS edge
                   ON edge.root_session_id = parent.root_session_id
                  AND edge.parent_session_id = parent.session_id
             )
             SELECT root_session_id, stopped_session_id, after_append_position, cause
             FROM fenced_scope AS fence
             WHERE fence.session_id = ?1
               AND ?2 <= fence.after_append_position
             ORDER BY fence.after_append_position ASC, fence.fence_rowid ASC
             LIMIT 1",
            params![session_id.to_string(), append_position],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    stored
        .map(
            |(root_session_id, stopped_session_id, after_append_position, cause)| {
                Ok(ApplicableAgentTreeStopFence {
                    root_session_id: parse_session_id_text(
                        &root_session_id,
                        "applicable tree-stop fence root",
                    )?,
                    stopped_session_id: parse_session_id_text(
                        &stopped_session_id,
                        "applicable tree-stop fence target",
                    )?,
                    after_append_position,
                    cause: AgentTreeStopFenceCause::parse(&cause)?,
                })
            },
        )
        .transpose()
}

fn terminal_is_compatible_with_tree_stop_fence(
    session_id: SessionId,
    terminal: &DurableTurnTerminal,
    fence: ApplicableAgentTreeStopFence,
) -> bool {
    if session_id != fence.stopped_session_id {
        return matches!(
            terminal.outcome,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::TreeStopped
            }
        );
    }
    matches!(
        (fence.cause, &terminal.outcome),
        (
            AgentTreeStopFenceCause::ApprovalAborted,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::ApprovalAborted
            }
        ) | (
            AgentTreeStopFenceCause::UserStop,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop
            }
        ) | (
            AgentTreeStopFenceCause::TreeStopped,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::TreeStopped
            }
        ) | (
            AgentTreeStopFenceCause::RootFailed,
            TurnTerminalOutcome::Failed { .. }
        )
    )
}

fn recovery_terminal_outcome_for_tree_stop_fence(
    session_id: SessionId,
    fence: ApplicableAgentTreeStopFence,
) -> TurnTerminalOutcome {
    if let Some(cause) = tree_stop_interruption_cause_for_fence(session_id, fence) {
        return TurnTerminalOutcome::Interrupted { cause };
    }
    match fence.cause {
        AgentTreeStopFenceCause::RootFailed => TurnTerminalOutcome::Failed {
            error: EXPIRED_RUN_RECOVERY_REASON.to_string(),
        },
        AgentTreeStopFenceCause::ApprovalAborted
        | AgentTreeStopFenceCause::UserStop
        | AgentTreeStopFenceCause::TreeStopped => {
            unreachable!("non-failure tree Stop causes map to typed interruptions")
        }
    }
}

fn tree_stop_interruption_cause_for_fence(
    session_id: SessionId,
    fence: ApplicableAgentTreeStopFence,
) -> Option<crate::protocol::TurnInterruptionCause> {
    if session_id != fence.stopped_session_id {
        return Some(crate::protocol::TurnInterruptionCause::TreeStopped);
    }
    match fence.cause {
        AgentTreeStopFenceCause::ApprovalAborted => {
            Some(crate::protocol::TurnInterruptionCause::ApprovalAborted)
        }
        AgentTreeStopFenceCause::UserStop => Some(crate::protocol::TurnInterruptionCause::UserStop),
        AgentTreeStopFenceCause::TreeStopped => {
            Some(crate::protocol::TurnInterruptionCause::TreeStopped)
        }
        AgentTreeStopFenceCause::RootFailed => None,
    }
}

fn runtime_state_admission_started_before_tree_stop_fence(
    connection: &Connection,
    session_id: SessionId,
    runtime_state: ValidatedSessionRuntimeState,
) -> Result<bool, StorageError> {
    match runtime_state.admission {
        Some(admission) => turn_started_before_applicable_tree_stop_fence_in_transaction(
            connection,
            session_id,
            admission.turn_id,
        ),
        None => Ok(false),
    }
}

fn discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
    transaction: &Transaction<'_>,
    resolver_session_id: SessionId,
    resolver_terminal_event_id: RuntimeEventId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    transaction
        .execute(
            "WITH RECURSIVE owners(session_id) AS (
                 SELECT ?1
                 UNION ALL
                 SELECT edge.parent_session_id
                 FROM session_spawn_edges AS edge
                 INNER JOIN owners AS child
                   ON edge.child_session_id = child.session_id
             )
             UPDATE agent_deferred_completions
             SET state = 'discarded',
                 resolved_by_terminal_event_id = ?2,
                 resolved_at_ms = ?3,
                 updated_at_ms = MAX(updated_at_ms, ?3)
             WHERE agent_session_id IN (SELECT session_id FROM owners)
               AND state = 'pending'
               AND EXISTS (
                   WITH RECURSIVE fenced_scope(
                       root_session_id,
                       stopped_session_id,
                       after_append_position,
                       session_id
                   ) AS (
                       SELECT
                           fence.root_session_id,
                           fence.stopped_session_id,
                           fence.after_append_position,
                           fence.stopped_session_id
                       FROM agent_tree_stop_fences AS fence
                       UNION ALL
                       SELECT
                           parent.root_session_id,
                           parent.stopped_session_id,
                           parent.after_append_position,
                           edge.child_session_id
                       FROM fenced_scope AS parent
                       INNER JOIN session_spawn_edges AS edge
                         ON edge.root_session_id = parent.root_session_id
                        AND edge.parent_session_id = parent.session_id
                   )
                   SELECT 1
                   FROM fenced_scope AS fence
                   INNER JOIN protocol_runtime_events AS resolver
                     ON resolver.id = ?2
                    AND resolver.session_id = fence.session_id
                    AND json_extract(resolver.msg_json, '$.kind') =
                        'turn_terminal'
                   INNER JOIN protocol_item_append_order AS resolver_order
                     ON resolver_order.session_id = resolver.session_id
                    AND resolver_order.source_kind = 'runtime_event'
                    AND resolver_order.source_id = resolver.id
                   WHERE (
                       SELECT MIN(deferred_turn_order.append_position)
                       FROM protocol_item_append_order AS deferred_turn_order
                       WHERE deferred_turn_order.session_id =
                             agent_deferred_completions.agent_session_id
                         AND deferred_turn_order.turn_id =
                             agent_deferred_completions.agent_turn_id
                   ) <= fence.after_append_position
                     AND resolver_order.append_position >
                         fence.after_append_position
               )",
            params![
                resolver_session_id.to_string(),
                resolver_terminal_event_id.to_string(),
                now_ms,
            ],
        )
        .map_err(StorageError::from)
}

fn append_agent_completion_handoff_in_transaction(
    transaction: &Transaction<'_>,
    child_session_id: SessionId,
    child_turn_id: TurnId,
    terminal: &DurableTurnTerminal,
    now: i64,
) -> Result<AgentCompletionHandoffDisposition, StorageError> {
    let lineage = transaction
        .query_row(
            "SELECT
                 edge.root_session_id,
                 edge.parent_session_id,
                 edge.agent_path,
                 CASE
                     WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                     ELSE parent_edge.agent_path
                 END
             FROM session_spawn_edges AS edge
             LEFT JOIN session_spawn_edges AS parent_edge
               ON parent_edge.root_session_id = edge.root_session_id
              AND parent_edge.child_session_id = edge.parent_session_id
             WHERE edge.child_session_id = ?1",
            params![child_session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((root_session_id, parent_session_id, child_agent_path, parent_agent_path)) = lineage
    else {
        return Ok(AgentCompletionHandoffDisposition::NotApplicable);
    };
    let parent_agent_path = parent_agent_path.ok_or_else(|| {
        StorageError::Message(format!(
            "child session {child_session_id} has no canonical immediate-parent agent path in root tree {root_session_id}"
        ))
    })?;
    let parent_session_id =
        parse_session_id_text(&parent_session_id, "agent completion handoff parent")?;
    let root_session_id = parse_session_id_text(&root_session_id, "agent completion handoff root")?;

    let payload = match &terminal.outcome {
        TurnTerminalOutcome::Interrupted { .. } => {
            return Ok(AgentCompletionHandoffDisposition::NotApplicable);
        }
        TurnTerminalOutcome::Completed => match terminal.final_response_id {
            Some(response_id) => AgentCompletionMessageContract::completed_payload(
                &exact_final_assistant_text_in_transaction(
                    transaction,
                    child_session_id,
                    child_turn_id,
                    response_id,
                )?,
            ),
            None => String::new(),
        },
        TurnTerminalOutcome::Failed { error } => {
            AgentCompletionMessageContract::failed_payload(error)
        }
    };

    if turn_started_before_applicable_tree_stop_fence_in_transaction(
        transaction,
        child_session_id,
        child_turn_id,
    )? {
        return Ok(AgentCompletionHandoffDisposition::SuppressedByTreeStop);
    }

    let communication = InterAgentCommunication {
        author: child_agent_path.clone(),
        recipient: parent_agent_path.clone(),
        content: render_inter_agent_message(
            InterAgentMessageType::FinalAnswer,
            &parent_agent_path,
            &child_agent_path,
            &payload,
        ),
        trigger_turn: false,
    };
    let history_item_id = insert_agent_mailbox_message_in_transaction(
        transaction,
        root_session_id,
        child_session_id,
        parent_session_id,
        communication,
        now,
        false,
    )?;

    let child_terminal_event_id =
        exact_terminal_event_id_in_transaction(transaction, child_session_id, child_turn_id)?;
    transaction.execute(
        "INSERT INTO agent_completion_handoffs (
             child_session_id,
             child_turn_id,
             child_terminal_event_id,
             parent_session_id,
             parent_history_item_id,
             created_at_ms
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            child_session_id.to_string(),
            child_turn_id.to_string(),
            child_terminal_event_id.to_string(),
            parent_session_id.to_string(),
            history_item_id.to_string(),
            now,
        ],
    )?;
    let released_owner_deferred_turn_id = supersede_pending_deferred_completion_in_transaction(
        transaction,
        parent_session_id,
        child_terminal_event_id,
        now,
    )?;
    let stored_handoff = StoredAgentCompletionHandoff {
        child_session_id,
        child_turn_id,
        parent_session_id,
        parent_agent_path: parent_agent_path.parse::<AgentPath>().map_err(|error| {
            StorageError::Message(format!(
                "agent completion handoff has invalid parent path `{parent_agent_path}`: {error}"
            ))
        })?,
        history_item_id,
        released_owner_deferred_turn_id,
    };
    schedule_owner_resume_for_released_deferred_handoff_in_transaction(
        transaction,
        Some(&stored_handoff),
        now,
    )?;
    Ok(AgentCompletionHandoffDisposition::Stored(stored_handoff))
}

fn resolve_agent_completion_handoff_disposition_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    disposition: AgentCompletionHandoffDisposition,
    now_ms: i64,
) -> Result<Option<StoredAgentCompletionHandoff>, StorageError> {
    match disposition {
        AgentCompletionHandoffDisposition::Stored(handoff) => Ok(Some(handoff)),
        AgentCompletionHandoffDisposition::NotApplicable => Ok(None),
        AgentCompletionHandoffDisposition::SuppressedByTreeStop => {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(transaction, session_id, turn_id)?;
            discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
                transaction,
                session_id,
                resolver_terminal_event_id,
                now_ms,
            )?;
            Ok(None)
        }
    }
}

fn release_quiescent_deferred_completions_after_interruption_in_transaction(
    transaction: &Transaction<'_>,
    interrupted_session_id: SessionId,
    interrupted_turn_id: TurnId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    let resolver_terminal_event_id = exact_terminal_event_id_in_transaction(
        transaction,
        interrupted_session_id,
        interrupted_turn_id,
    )?;
    if turn_started_before_applicable_tree_stop_fence_in_transaction(
        transaction,
        interrupted_session_id,
        interrupted_turn_id,
    )? {
        return discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
            transaction,
            interrupted_session_id,
            resolver_terminal_event_id,
            now_ms,
        );
    }
    let mut statement = transaction.prepare(
        "WITH RECURSIVE ancestors(session_id, depth) AS (
             SELECT edge.parent_session_id, 1
             FROM session_spawn_edges AS edge
             WHERE edge.child_session_id = ?1
             UNION ALL
             SELECT edge.parent_session_id, parent.depth + 1
             FROM session_spawn_edges AS edge
             INNER JOIN ancestors AS parent
               ON edge.child_session_id = parent.session_id
         )
         SELECT deferred.agent_session_id, deferred.agent_turn_id
         FROM ancestors
         INNER JOIN agent_deferred_completions AS deferred
           ON deferred.agent_session_id = ancestors.session_id
          AND deferred.state = 'pending'
         ORDER BY ancestors.depth ASC",
    )?;
    let candidates = statement
        .query_map(params![interrupted_session_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let mut released = 0usize;
    for (agent_session_id, agent_turn_id) in candidates {
        let agent_session_id = parse_session_id_text(&agent_session_id, "deferred release agent")?;
        let agent_turn_id = agent_turn_id.parse::<TurnId>().map_err(|error| {
            StorageError::Message(format!(
                "deferred release has invalid agent turn id `{agent_turn_id}`: {error}"
            ))
        })?;
        let still_pending = transaction.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM agent_deferred_completions
                 WHERE agent_session_id = ?1
                   AND agent_turn_id = ?2
                   AND state = 'pending'
             )",
            params![agent_session_id.to_string(), agent_turn_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?;
        if !still_pending {
            continue;
        }
        if session_has_durable_descendant_work_in_connection(transaction, agent_session_id)? {
            continue;
        }
        let terminal = terminal_for_turn_in_connection(
            transaction,
            agent_session_id,
            agent_turn_id,
        )?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "pending deferred completion {agent_session_id} turn {agent_turn_id} has no durable terminal"
            ))
        })?;
        match append_agent_completion_handoff_in_transaction(
            transaction,
            agent_session_id,
            agent_turn_id,
            &terminal,
            now_ms,
        )? {
            AgentCompletionHandoffDisposition::Stored(_) => {}
            AgentCompletionHandoffDisposition::SuppressedByTreeStop => {
                let discarded =
                    discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
                        transaction,
                        interrupted_session_id,
                        resolver_terminal_event_id,
                        now_ms,
                    )?;
                if discarded == 0 {
                    return Err(StorageError::Message(format!(
                        "tree-stop-suppressed deferred completion {agent_session_id} was not resolved by descendant {interrupted_session_id}"
                    )));
                }
                continue;
            }
            AgentCompletionHandoffDisposition::NotApplicable => {
                return Err(StorageError::Message(format!(
                    "pending deferred completion {agent_session_id} is not a retained non-root agent"
                )));
            }
        }
        let updated = transaction.execute(
            "UPDATE agent_deferred_completions
             SET state = 'released',
                 resolved_by_terminal_event_id = ?3,
                 resolved_at_ms = ?4,
                 updated_at_ms = MAX(updated_at_ms, ?4)
             WHERE agent_session_id = ?1
               AND agent_turn_id = ?2
               AND state = 'pending'",
            params![
                agent_session_id.to_string(),
                agent_turn_id.to_string(),
                resolver_terminal_event_id.to_string(),
                now_ms,
            ],
        )?;
        if updated != 1 {
            return Err(StorageError::Message(format!(
                "deferred completion {agent_session_id} turn {agent_turn_id} changed while its release transaction held the writer lock"
            )));
        }
        released = released.saturating_add(1);
    }
    Ok(released)
}

fn truncate_agent_completion_middle(value: &str, max_tokens: usize) -> String {
    const APPROX_BYTES_PER_TOKEN: usize = 4;

    let max_bytes = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if value.len() <= max_bytes {
        return value.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }

    let mut marker = agent_completion_truncation_marker(
        value.len().saturating_sub(max_bytes),
        APPROX_BYTES_PER_TOKEN,
    );
    for _ in 0..8 {
        let content_budget = max_bytes.saturating_sub(marker.len());
        let prefix_budget = content_budget / 2;
        let suffix_budget = content_budget.saturating_sub(prefix_budget);
        let prefix = utf8_prefix_within_bytes(value, prefix_budget);
        let suffix = utf8_suffix_within_bytes(&value[prefix.len()..], suffix_budget);
        let removed_bytes = value
            .len()
            .saturating_sub(prefix.len())
            .saturating_sub(suffix.len());
        let next_marker = agent_completion_truncation_marker(removed_bytes, APPROX_BYTES_PER_TOKEN);
        if next_marker == marker {
            let mut truncated = String::with_capacity(prefix.len() + marker.len() + suffix.len());
            truncated.push_str(prefix);
            truncated.push_str(&marker);
            truncated.push_str(suffix);
            debug_assert!(truncated.len() <= max_bytes);
            return truncated;
        }
        marker = next_marker;
    }

    let content_budget = max_bytes.saturating_sub(marker.len());
    let prefix_budget = content_budget / 2;
    let suffix_budget = content_budget.saturating_sub(prefix_budget);
    let prefix = utf8_prefix_within_bytes(value, prefix_budget);
    let suffix = utf8_suffix_within_bytes(&value[prefix.len()..], suffix_budget);
    let mut truncated = String::with_capacity(prefix.len() + marker.len() + suffix.len());
    truncated.push_str(prefix);
    truncated.push_str(&marker);
    truncated.push_str(suffix);
    debug_assert!(truncated.len() <= max_bytes);
    truncated
}

fn agent_completion_truncation_marker(
    removed_bytes: usize,
    approx_bytes_per_token: usize,
) -> String {
    let removed_tokens = removed_bytes.div_ceil(approx_bytes_per_token);
    format!("…{removed_tokens} tokens truncated…")
}

fn utf8_prefix_within_bytes(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn utf8_suffix_within_bytes(value: &str, max_bytes: usize) -> &str {
    let mut start = value.len().saturating_sub(max_bytes);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

fn exact_terminal_event_id_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<RuntimeEventId, StorageError> {
    let mut statement = transaction.prepare(
        "SELECT id
         FROM protocol_runtime_events
         WHERE session_id = ?1
           AND turn_id = ?2
           AND json_extract(msg_json, '$.kind') = 'turn_terminal'
         ORDER BY sequence_no DESC, rowid DESC
         LIMIT 2",
    )?;
    let mut rows = statement.query_map(
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    let Some(id) = rows.next() else {
        return Err(StorageError::Message(format!(
            "agent completion handoff for session {session_id} turn {turn_id} has no durable terminal event"
        )));
    };
    let id = id?;
    if rows.next().transpose()?.is_some() {
        return Err(StorageError::Message(format!(
            "agent completion handoff for session {session_id} turn {turn_id} found duplicate terminal events"
        )));
    }
    id.parse::<RuntimeEventId>().map_err(|error| {
        StorageError::Message(format!(
            "agent completion handoff terminal event has invalid id `{id}`: {error}"
        ))
    })
}

fn exact_final_assistant_text_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    response_id: ModelResponseId,
) -> Result<String, StorageError> {
    let mut statement = transaction.prepare(
        "SELECT payload_json
         FROM protocol_history_items
         WHERE session_id = ?1
           AND scope_kind = 'turn'
           AND turn_id = ?2
           AND json_extract(payload_json, '$.kind') = 'assistant_message'
           AND json_extract(payload_json, '$.response_id') = ?3
         ORDER BY sequence_no DESC, rowid DESC
         LIMIT 2",
    )?;
    let mut rows = statement.query_map(
        params![
            session_id.to_string(),
            turn_id.to_string(),
            response_id.to_string()
        ],
        |row| row.get::<_, String>(0),
    )?;
    let Some(payload_json) = rows.next() else {
        return Err(StorageError::Message(format!(
            "completed child session {session_id} turn {turn_id} references missing final response {response_id}"
        )));
    };
    let payload_json = payload_json?;
    if rows.next().transpose()?.is_some() {
        return Err(StorageError::Message(format!(
            "completed child session {session_id} turn {turn_id} has duplicate assistant history for final response {response_id}"
        )));
    }
    let HistoryItemPayload::AssistantMessage {
        response_id: stored_response_id,
        content,
    } = serde_json::from_str::<HistoryItemPayload>(&payload_json)?
    else {
        return Err(StorageError::Message(format!(
            "completed child session {session_id} turn {turn_id} final response {response_id} is not assistant history"
        )));
    };
    if stored_response_id != response_id {
        return Err(StorageError::Message(format!(
            "completed child session {session_id} turn {turn_id} final response identity changed while decoding"
        )));
    }
    let tool_call_count = transaction.query_row(
        "SELECT COUNT(*)
         FROM protocol_history_items
         WHERE session_id = ?1
           AND scope_kind = 'turn'
           AND turn_id = ?2
           AND json_extract(payload_json, '$.kind') = 'tool_call'
           AND json_extract(payload_json, '$.response_id') = ?3",
        params![
            session_id.to_string(),
            turn_id.to_string(),
            response_id.to_string()
        ],
        |row| row.get::<_, i64>(0),
    )?;
    if tool_call_count != 0 {
        return Err(StorageError::Message(format!(
            "completed child session {session_id} turn {turn_id} final response {response_id} contains tool calls instead of one final answer"
        )));
    }
    let mut text_parts = Vec::with_capacity(content.len());
    for part in &content {
        match part {
            crate::protocol::ContentPart::Text { text } => text_parts.push(text.as_str()),
            crate::protocol::ContentPart::Image { .. } => {
                return Err(StorageError::Message(format!(
                    "completed child session {session_id} turn {turn_id} final response {response_id} contains non-text assistant content"
                )));
            }
        }
    }
    Ok(text_parts.join("\n"))
}

fn terminal_for_retained_admission_in_connection(
    connection: &Connection,
    session_id: SessionId,
    session_status: SessionStatus,
    admission: DurableRunAdmission,
) -> Result<DurableTurnTerminal, StorageError> {
    let terminal = terminal_for_turn_in_connection(connection, session_id, admission.turn_id)?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "terminal session {session_id} retained admission {} for turn {} without a durable terminal",
                admission.admission_id, admission.turn_id
            ))
        })?;
    if terminal.session_status() != session_status {
        return Err(StorageError::Message(format!(
            "session {session_id} status {} contradicts durable terminal status {} for turn {}",
            session_status_text(session_status),
            session_status_text(terminal.session_status()),
            admission.turn_id
        )));
    }
    Ok(terminal)
}

fn validate_retained_admission_terminal_state_in_connection(
    connection: &Connection,
    session_id: SessionId,
    runtime_state: ValidatedSessionRuntimeState,
) -> Result<Option<DurableTurnTerminal>, StorageError> {
    let Some(admission) = runtime_state.admission else {
        return Ok(None);
    };
    match runtime_state.status {
        SessionStatus::Running => {
            if terminal_for_turn_in_connection(connection, session_id, admission.turn_id)?.is_some()
            {
                return Err(StorageError::Message(format!(
                    "running session {session_id} active turn {} already has a durable terminal",
                    admission.turn_id
                )));
            }
            Ok(None)
        }
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed => {
            terminal_for_retained_admission_in_connection(
                connection,
                session_id,
                runtime_state.status,
                admission,
            )
            .map(Some)
        }
        SessionStatus::Idle => Err(StorageError::Message(format!(
            "idle session {session_id} unexpectedly retains a durable run admission"
        ))),
    }
}

fn parse_status(value: &str) -> Result<SessionStatus, StorageError> {
    match value {
        "idle" => Ok(SessionStatus::Idle),
        "running" => Ok(SessionStatus::Running),
        "completed" => Ok(SessionStatus::Completed),
        "cancelled" => Ok(SessionStatus::Cancelled),
        "failed" => Ok(SessionStatus::Failed),
        _ => Err(StorageError::Message(format!(
            "unknown persisted session status `{value}`"
        ))),
    }
}

fn parse_session_id_text(value: &str, context: &str) -> Result<SessionId, StorageError> {
    value.parse::<SessionId>().map_err(|error| {
        StorageError::Message(format!(
            "{context} has invalid session id `{value}`: {error}"
        ))
    })
}

fn parse_status_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<SessionStatus> {
    let value = row.get::<_, String>(index)?;
    parse_status(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

struct StoredThreadGoal {
    goal: ThreadGoal,
    goal_id: String,
    updated_at_ms: i64,
}

fn stored_thread_goal_from_connection(
    connection: &Connection,
    thread_id: SessionId,
) -> Result<Option<StoredThreadGoal>, StorageError> {
    let row = connection
        .query_row(
            "SELECT thread_id, goal_id, objective, status, token_budget, tokens_used,
                    time_used_seconds, created_at_ms, updated_at_ms
             FROM thread_goals
             WHERE thread_id = ?1",
            params![thread_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    row.map(stored_thread_goal_from_row).transpose()
}

fn set_thread_goal_objective_in_transaction(
    transaction: &Transaction<'_>,
    thread_id: SessionId,
    objective: &str,
    now_ms: i64,
) -> Result<(), StorageError> {
    let objective = objective.trim();
    let stored = stored_thread_goal_from_connection(transaction, thread_id)?;
    match stored {
        Some(stored) => {
            validate_goal_objective_and_budget(objective, stored.goal.token_budget)?;
            let elapsed_seconds = if matches!(
                stored.goal.status,
                ThreadGoalStatus::Active | ThreadGoalStatus::BudgetLimited
            ) {
                now_ms.saturating_sub(stored.updated_at_ms).max(0) / 1_000
            } else {
                0
            };
            let time_used_seconds = stored
                .goal
                .time_used_seconds
                .saturating_add(elapsed_seconds);
            let status = status_after_budget_limit(
                ThreadGoalStatus::Active,
                stored.goal.tokens_used,
                stored.goal.token_budget,
            );
            let updated_at_ms = now_ms.max(stored.updated_at_ms.saturating_add(1));
            let changed = transaction.execute(
                "UPDATE thread_goals
                 SET objective = ?2,
                     status = ?3,
                     time_used_seconds = ?4,
                     updated_at_ms = ?5
                 WHERE thread_id = ?1
                   AND goal_id = ?6
                   AND updated_at_ms = ?7",
                params![
                    thread_id.to_string(),
                    objective,
                    status.as_db_str(),
                    time_used_seconds,
                    updated_at_ms,
                    stored.goal_id,
                    stored.updated_at_ms,
                ],
            )?;
            if changed != 1 {
                return Err(StorageError::Message(
                    "thread goal changed while admitting its owning turn".to_string(),
                ));
            }
        }
        None => {
            validate_goal_objective_and_budget(objective, None)?;
            transaction.execute(
                "INSERT INTO thread_goals (
                     thread_id, goal_id, objective, status, token_budget, tokens_used,
                     time_used_seconds, created_at_ms, updated_at_ms
                 )
                 VALUES (?1, ?2, ?3, 'active', NULL, 0, 0, ?4, ?4)",
                params![
                    thread_id.to_string(),
                    ulid::Ulid::new().to_string(),
                    objective,
                    now_ms,
                ],
            )?;
        }
    }
    Ok(())
}

fn stored_thread_goal_from_row(
    row: (
        String,
        String,
        String,
        String,
        Option<i64>,
        i64,
        i64,
        i64,
        i64,
    ),
) -> Result<StoredThreadGoal, StorageError> {
    let (
        thread_id,
        goal_id,
        objective,
        status,
        token_budget,
        tokens_used,
        time_used_seconds,
        created_at_ms,
        updated_at_ms,
    ) = row;
    let thread_id = thread_id
        .parse::<SessionId>()
        .map_err(|error| StorageError::Message(format!("invalid thread goal id: {error}")))?;
    let status = ThreadGoalStatus::parse_db(&status).ok_or_else(|| {
        StorageError::Message(format!("invalid thread goal status `{status}` in storage"))
    })?;
    Ok(StoredThreadGoal {
        goal: ThreadGoal {
            thread_id,
            objective,
            status,
            token_budget,
            tokens_used,
            time_used_seconds,
            created_at: created_at_ms / 1000,
            updated_at: updated_at_ms / 1000,
        },
        goal_id,
        updated_at_ms,
    })
}

fn validate_goal_objective_and_budget(
    objective: &str,
    token_budget: Option<i64>,
) -> Result<(), StorageError> {
    validate_thread_goal_objective(objective).map_err(StorageError::Message)?;
    if token_budget.is_some_and(|budget| budget <= 0) {
        return Err(StorageError::Message(
            "goal token budget must be positive".to_string(),
        ));
    }
    Ok(())
}

fn status_after_budget_limit(
    status: ThreadGoalStatus,
    tokens_used: i64,
    token_budget: Option<i64>,
) -> ThreadGoalStatus {
    if token_budget.is_some_and(|budget| tokens_used >= budget) {
        ThreadGoalStatus::BudgetLimited
    } else {
        status
    }
}

fn parse_access_mode_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<AccessMode> {
    let value = row.get::<_, String>(index)?;
    match value.as_str() {
        "default" => Ok(AccessMode::Default),
        "auto_review" => Ok(AccessMode::AutoReview),
        "full_access" => Ok(AccessMode::FullAccess),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown persisted access mode `{value}`"),
            )),
        )),
    }
}

fn parse_session_model_parameters(
    value: &str,
    column: usize,
) -> Result<SessionModelParameters, rusqlite::Error> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn insert_protocol_projection_if_requested(
    transaction: &rusqlite::Transaction<'_>,
    event: &RunEvent,
    fallback_session_id: Option<SessionId>,
    protocol_turn_id: TurnId,
    protocol_sequence_no: Option<i64>,
) -> Result<(), StorageError> {
    let protocol_sequence_no = protocol_sequence_no.unwrap_or(0);
    let Some(projection) = project_protocol_run_event(
        event,
        fallback_session_id,
        protocol_turn_id,
        protocol_sequence_no,
    ) else {
        return Ok(());
    };
    crate::protocol::insert_session_owned_event_bundle_in_transaction(
        &SESSION_PROTOCOL_WRITE_AUTHORITY,
        transaction,
        &projection.runtime_event,
        projection.history_item.as_ref(),
        projection.turn_item.as_ref(),
    )?;
    Ok(())
}

fn validate_canonical_tool_call_payload(
    history_item: &HistoryItem,
    tool_call_id: ToolCallId,
) -> Result<(), StorageError> {
    match &history_item.payload {
        HistoryItemPayload::ToolCall { call_id, .. } if *call_id == tool_call_id => Ok(()),
        HistoryItemPayload::ToolCall { call_id, .. } => Err(StorageError::Message(format!(
            "canonical tool call identity mismatch: expected {tool_call_id} got {call_id}",
        ))),
        _ => Err(StorageError::Message(
            "tool sidecar must reference a canonical ToolCall history item".to_string(),
        )),
    }
}

fn validate_canonical_tool_call_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    tool_call_id: ToolCallId,
    tool_name: crate::tool::ToolName,
) -> Result<HistoryItemId, StorageError> {
    let stored = transaction
        .query_row(
            "SELECT history.id, history.sequence_no, history.payload_json, history.created_at_ms
             FROM tool_calls AS tool
             INNER JOIN protocol_history_items AS history
                ON history.id = tool.history_item_id
             WHERE tool.id = ?1 AND history.session_id = ?2 AND history.turn_id = ?3",
            params![
                tool_call_id.to_string(),
                session_id.to_string(),
                turn_id.to_string(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((history_item_id, sequence_no, payload_json, created_at_ms)) = stored else {
        return Err(StorageError::Message(format!(
            "tool call {tool_call_id} is not owned by session {session_id} turn {turn_id}"
        )));
    };
    let history_item = HistoryItem {
        id: history_item_id.parse::<HistoryItemId>().map_err(|error| {
            StorageError::Message(format!("invalid tool history item id: {error}"))
        })?,
        session_id,
        scope: crate::protocol::HistoryScope::Turn { turn_id },
        sequence_no,
        created_at_ms,
        payload: serde_json::from_str(&payload_json)?,
    };
    validate_canonical_tool_call_payload(&history_item, tool_call_id)?;
    let HistoryItemPayload::ToolCall {
        tool_name: stored_tool_name,
        ..
    } = &history_item.payload
    else {
        unreachable!("canonical payload validation accepted a non-tool-call item");
    };
    let stored_tool = crate::tool::ToolName::parse(stored_tool_name);
    if stored_tool != tool_name {
        return Err(StorageError::Message(format!(
            "canonical tool call name mismatch: expected {tool_name} got raw `{stored_tool_name}` ({stored_tool})"
        )));
    }
    Ok(history_item.id)
}

fn validate_persisted_file_change_ownership(
    transaction: &Transaction<'_>,
    tool_call_id: ToolCallId,
    file_changes: &[crate::edit::ChangeSummary],
) -> Result<(), StorageError> {
    let mut seen = HashSet::with_capacity(file_changes.len());
    let tool_call_id_text = tool_call_id.to_string();
    for change in file_changes {
        if !seen.insert(change.change_id) {
            return Err(StorageError::Message(format!(
                "file change {} is duplicated in one tool settlement",
                change.change_id
            )));
        }
        let owner = transaction
            .query_row(
                "SELECT tool_call_id FROM file_changes WHERE id = ?1",
                params![change.change_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if owner.as_deref() != Some(tool_call_id_text.as_str()) {
            return Err(StorageError::Message(format!(
                "file change {} is not durable evidence for tool call {tool_call_id}",
                change.change_id
            )));
        }
    }
    Ok(())
}

fn session_status_text(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Completed => "completed",
        SessionStatus::Cancelled => "cancelled",
        SessionStatus::Failed => "failed",
    }
}

fn raw_session_runtime_state_from_row(
    row: &rusqlite::Row<'_>,
    first_column: usize,
) -> rusqlite::Result<RawSessionRuntimeState> {
    Ok(RawSessionRuntimeState {
        status: row.get(first_column)?,
        active_run_id: row.get(first_column + 1)?,
        active_turn_id: row.get(first_column + 2)?,
        active_run_lease_expires_at_ms: row.get(first_column + 3)?,
        terminal_count: row.get(first_column + 4)?,
        terminal_json: row.get(first_column + 5)?,
    })
}

fn validate_raw_session_runtime_state(
    session_id: SessionId,
    raw: RawSessionRuntimeState,
) -> Result<ValidatedSessionRuntimeState, StorageError> {
    let runtime_state = parse_session_runtime_state(
        session_id,
        &raw.status,
        raw.active_run_id.as_deref(),
        raw.active_turn_id.as_deref(),
        raw.active_run_lease_expires_at_ms,
    )?;
    let terminal = terminal_from_same_statement_evidence(
        session_id,
        runtime_state.admission.map(|admission| admission.turn_id),
        raw.terminal_count,
        raw.terminal_json.as_deref(),
    )?;
    if let Some(admission) = runtime_state.admission {
        match runtime_state.status {
            SessionStatus::Running if terminal.is_some() => {
                return Err(StorageError::Message(format!(
                    "running session {session_id} active turn {} already has a durable terminal",
                    admission.turn_id
                )));
            }
            SessionStatus::Running => {}
            SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed => {
                let terminal = terminal.ok_or_else(|| {
                    StorageError::Message(format!(
                        "terminal session {session_id} retained admission {} for turn {} without a durable terminal",
                        admission.admission_id, admission.turn_id
                    ))
                })?;
                if terminal.session_status() != runtime_state.status {
                    return Err(StorageError::Message(format!(
                        "session {session_id} status {} contradicts durable terminal status {} for turn {}",
                        session_status_text(runtime_state.status),
                        session_status_text(terminal.session_status()),
                        admission.turn_id
                    )));
                }
            }
            SessionStatus::Idle => {
                return Err(StorageError::Message(format!(
                    "idle session {session_id} unexpectedly retains a durable run admission"
                )));
            }
        }
    }
    Ok(runtime_state)
}

fn terminal_from_same_statement_evidence(
    session_id: SessionId,
    turn_id: Option<TurnId>,
    terminal_count: i64,
    terminal_json: Option<&str>,
) -> Result<Option<DurableTurnTerminal>, StorageError> {
    let turn_label = turn_id
        .map(|turn_id| turn_id.to_string())
        .unwrap_or_else(|| "<none>".to_string());
    match (terminal_count, terminal_json) {
        (0, None) => Ok(None),
        (1, Some(terminal_json)) => {
            let RuntimeEventMsg::TurnTerminal { terminal } =
                serde_json::from_str::<RuntimeEventMsg>(terminal_json)?
            else {
                return Err(StorageError::Message(
                    "terminal runtime-event discriminator did not decode as TurnTerminal"
                        .to_string(),
                ));
            };
            Ok(Some(*terminal))
        }
        (count, _) if count > 1 => Err(StorageError::Message(format!(
            "multiple durable terminals exist for session {session_id} turn {turn_label}"
        ))),
        (count, _) => Err(StorageError::Message(format!(
            "terminal evidence count/payload mismatch for session {session_id} turn {turn_label}: count {count}"
        ))),
    }
}

fn session_runtime_state_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Option<ValidatedSessionRuntimeState>, StorageError> {
    let raw = connection
        .query_row(
            "SELECT status, active_run_id, active_turn_id, active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1)
             FROM sessions
             WHERE id = ?1",
            params![session_id.to_string()],
            |row| raw_session_runtime_state_from_row(row, 0),
        )
        .optional()?;
    raw.map(|raw| validate_raw_session_runtime_state(session_id, raw))
        .transpose()
}

fn ensure_turn_identity_unused_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<(), StorageError> {
    let used = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM protocol_history_items
             WHERE session_id = ?1 AND scope_kind = 'turn' AND turn_id = ?2
             UNION ALL
             SELECT 1
             FROM protocol_turn_items
             WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL
             SELECT 1
             FROM protocol_runtime_events
             WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL
             SELECT 1
             FROM protocol_item_append_order
             WHERE session_id = ?1 AND scope_kind = 'turn' AND turn_id = ?2
             UNION ALL
             SELECT 1
             FROM protocol_turn_sequence_allocators
             WHERE session_id = ?1 AND turn_id = ?2
             LIMIT 1
         )",
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if used {
        return Err(StorageError::Message(format!(
            "turn identity {turn_id} has already been used by session {session_id}"
        )));
    }
    Ok(())
}

fn parse_session_runtime_state(
    session_id: SessionId,
    status: &str,
    active_run_id: Option<&str>,
    active_turn_id: Option<&str>,
    lease_expires_at_ms: Option<i64>,
) -> Result<ValidatedSessionRuntimeState, StorageError> {
    let status = parse_status(status)?;
    let admission = match (active_run_id, active_turn_id, lease_expires_at_ms) {
        (None, None, None) if status != SessionStatus::Running => None,
        (None, None, None) => {
            return Err(StorageError::Message(format!(
                "running session {session_id} has no durable run admission or active turn"
            )));
        }
        (Some(run_id), Some(turn_id), Some(lease_expires_at_ms)) if lease_expires_at_ms > 0 => {
            if status == SessionStatus::Idle {
                return Err(StorageError::Message(format!(
                    "idle session {session_id} unexpectedly retains a durable run admission"
                )));
            }
            let admission_id = run_id.parse::<AdmissionId>().map_err(|_| {
                StorageError::Message(format!(
                    "session {session_id} has an invalid durable run admission identity"
                ))
            })?;
            let turn_id = turn_id.parse::<TurnId>().map_err(|_| {
                StorageError::Message(format!(
                    "session {session_id} has an invalid durable active turn identity"
                ))
            })?;
            Some(DurableRunAdmission {
                admission_id,
                turn_id,
                lease_expires_at_ms,
            })
        }
        _ => {
            return Err(StorageError::Message(format!(
                "session {session_id} has an incomplete durable run admission"
            )));
        }
    };
    Ok(ValidatedSessionRuntimeState { status, admission })
}

fn deliver_pending_agent_mail_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    selector: AgentMailboxDeliverySelector,
    limit: usize,
    now_ms: i64,
) -> Result<DeliveredAgentMailboxPage, StorageError> {
    let limit = limit.clamp(1, 128);
    let query_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(129);
    let required_child_results_only =
        i64::from(selector == AgentMailboxDeliverySelector::RequiredChildResultsOnly);
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT mailbox.id, mailbox.payload_json
             FROM agent_mailbox_messages AS mailbox
             INNER JOIN protocol_item_append_order AS enqueue_order
               ON enqueue_order.session_id = mailbox.recipient_session_id
              AND enqueue_order.source_kind = 'mailbox_message'
              AND enqueue_order.source_id = mailbox.id
             WHERE mailbox.recipient_session_id = ?1
               AND mailbox.state = 'pending'
               AND (
                   ?2 = 0
                   OR EXISTS (
                       SELECT 1
                       FROM agent_completion_handoffs AS handoff
                       WHERE handoff.parent_session_id = mailbox.recipient_session_id
                         AND handoff.parent_history_item_id = mailbox.id
                   )
               )
             ORDER BY enqueue_order.append_position ASC
             LIMIT ?3",
        )?;
        statement
            .query_map(
                params![
                    session_id.to_string(),
                    required_child_results_only,
                    query_limit
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    let has_more = rows.len() > limit;
    let mut history_item_ids = Vec::with_capacity(rows.len().min(limit));
    for (message_id, payload_json) in rows.into_iter().take(limit) {
        history_item_ids.push(deliver_one_pending_agent_mail_in_transaction(
            transaction,
            session_id,
            turn_id,
            &message_id,
            &payload_json,
            now_ms,
        )?);
    }
    Ok(DeliveredAgentMailboxPage {
        history_item_ids,
        has_more,
    })
}

fn deliver_one_pending_agent_mail_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    message_id: &str,
    payload_json: &str,
    now_ms: i64,
) -> Result<HistoryItemId, StorageError> {
    let message_id = message_id.parse::<HistoryItemId>().map_err(|error| {
        StorageError::Message(format!(
            "agent mailbox message has invalid identity `{message_id}`: {error}"
        ))
    })?;
    let payload = serde_json::from_str::<HistoryItemPayload>(payload_json)?;
    let HistoryItemPayload::InterAgentCommunication { communication } = payload else {
        return Err(StorageError::Message(format!(
            "agent mailbox message {message_id} does not contain inter-agent communication"
        )));
    };
    let projection = project_inter_agent_communication_with_history_item_id(
        session_id,
        turn_id,
        0,
        message_id,
        communication,
    );
    let stored = insert_session_owned_event_bundle_in_transaction(
        &SESSION_PROTOCOL_WRITE_AUTHORITY,
        transaction,
        &projection.runtime_event,
        projection.history_item.as_ref(),
        projection.turn_item.as_ref(),
    )?;
    if stored.history_item.as_ref().map(|item| item.id) != Some(message_id) {
        return Err(StorageError::Message(format!(
            "agent mailbox delivery for {message_id} did not preserve canonical history identity"
        )));
    }
    let transitioned = transaction.execute(
        "UPDATE agent_mailbox_messages
         SET state = 'delivered',
             delivered_turn_id = ?2,
             delivered_history_item_id = id,
             updated_at_ms = MAX(updated_at_ms, ?3),
             resolved_at_ms = ?3
         WHERE id = ?1
           AND recipient_session_id = ?4
           AND state = 'pending'",
        params![
            message_id.to_string(),
            turn_id.to_string(),
            now_ms,
            session_id.to_string(),
        ],
    )?;
    if transitioned != 1 {
        return Err(StorageError::Message(format!(
            "agent mailbox message {message_id} lost its pending delivery owner"
        )));
    }
    Ok(message_id)
}

fn deliver_claimed_explicit_agent_wake_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    history_item_id: HistoryItemId,
    now_ms: i64,
) -> Result<(), StorageError> {
    let stored = transaction
        .query_row(
            "SELECT state, delivered_turn_id, payload_json
             FROM agent_mailbox_messages
             WHERE id = ?1
               AND recipient_session_id = ?2
               AND trigger_turn = 1",
            params![history_item_id.to_string(), session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "claimed explicit agent wake {history_item_id} no longer belongs to session {session_id}"
            ))
        })?;
    match stored {
        (state, _, payload_json) if state == "pending" => {
            let delivered = deliver_one_pending_agent_mail_in_transaction(
                transaction,
                session_id,
                turn_id,
                &history_item_id.to_string(),
                &payload_json,
                now_ms,
            )?;
            if delivered != history_item_id {
                return Err(StorageError::Message(format!(
                    "claimed explicit agent wake {history_item_id} delivered as {delivered}"
                )));
            }
            Ok(())
        }
        (state, Some(delivered_turn_id), _) if state == "delivered" => {
            let delivered_turn_id =
                delivered_turn_id.parse::<TurnId>().map_err(|error| {
                    StorageError::Message(format!(
                        "claimed explicit agent wake {history_item_id} has invalid delivered turn `{delivered_turn_id}`: {error}"
                    ))
                })?;
            if delivered_turn_id != turn_id {
                return Err(StorageError::Message(format!(
                    "claimed explicit agent wake {history_item_id} was delivered to turn {delivered_turn_id} instead of owner turn {turn_id}"
                )));
            }
            Ok(())
        }
        (state, _, _) => Err(StorageError::Message(format!(
            "claimed explicit agent wake {history_item_id} cannot complete turn {turn_id} from mailbox state `{state}`"
        ))),
    }
}

fn deliver_all_pending_agent_mail_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    now_ms: i64,
) -> Result<Vec<HistoryItemId>, StorageError> {
    let mut delivered = Vec::new();
    loop {
        let page = deliver_pending_agent_mail_in_transaction(
            transaction,
            session_id,
            turn_id,
            AgentMailboxDeliverySelector::AllPending,
            128,
            now_ms,
        )?;
        delivered.extend(page.history_item_ids);
        if !page.has_more {
            return Ok(delivered);
        }
    }
}

fn count_pending_agent_mailbox_messages(
    connection: &Connection,
    session_id: SessionId,
) -> Result<usize, StorageError> {
    let count = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_mailbox_messages
         WHERE recipient_session_id = ?1 AND state = 'pending'",
        params![session_id.to_string()],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).map_err(|_| {
        StorageError::Message(format!(
            "pending agent mailbox count for session {session_id} exceeds this platform's range"
        ))
    })
}

pub(crate) fn normalize_run_lease_now_ms(now_ms: i64) -> i64 {
    now_ms.clamp(0, i64::MAX - 1)
}

fn run_lease_expiry_ms(now_ms: i64, lease_duration_ms: i64) -> i64 {
    normalize_run_lease_now_ms(now_ms).saturating_add(lease_duration_ms.max(1))
}

fn schedulable_owner_resume_request_id_in_connection(
    connection: &Connection,
    owner_session_id: SessionId,
) -> Result<Option<OwnerResumeRequestId>, StorageError> {
    if has_unclaimed_agent_trigger_in_connection(connection, owner_session_id)? {
        return Ok(None);
    }
    oldest_pending_owner_resume_request_id_in_connection(connection, owner_session_id)
}

fn oldest_pending_owner_resume_request_id_in_connection(
    connection: &Connection,
    owner_session_id: SessionId,
) -> Result<Option<OwnerResumeRequestId>, StorageError> {
    connection
        .query_row(
            "SELECT source_history_item_id
             FROM agent_owner_resume_requests
             WHERE owner_session_id = ?1
               AND state = 'pending'
             ORDER BY created_at_ms ASC, source_history_item_id ASC
             LIMIT 1",
            params![owner_session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| {
            value.parse::<OwnerResumeRequestId>().map_err(|error| {
                StorageError::Message(format!(
                    "owner-resume request for session {owner_session_id} has invalid source history id `{value}`: {error}"
                ))
            })
        })
        .transpose()
}

fn deferred_agent_completion_in_connection(
    connection: &Connection,
    agent_session_id: SessionId,
    agent_turn_id: Option<TurnId>,
    state: Option<&'static str>,
) -> Result<Option<DeferredAgentCompletion>, StorageError> {
    let row = connection
        .query_row(
            "SELECT agent_session_id, agent_turn_id, parent_session_id, kind, state,
                    resolved_by_terminal_event_id
             FROM effective_agent_deferred_completions
             WHERE agent_session_id = ?1
               AND (?2 IS NULL OR agent_turn_id = ?2)
               AND (?3 IS NULL OR state = ?3)
             ORDER BY created_at_ms DESC, agent_turn_id DESC
             LIMIT 1",
            params![
                agent_session_id.to_string(),
                agent_turn_id.map(|turn_id| turn_id.to_string()),
                state,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((
        agent_session_id,
        agent_turn_id,
        parent_session_id,
        kind,
        state,
        resolved_by_terminal_event_id,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(DeferredAgentCompletion {
        agent_session_id: parse_session_id_text(
            &agent_session_id,
            "deferred-completion agent session",
        )?,
        agent_turn_id: agent_turn_id.parse::<TurnId>().map_err(|error| {
            StorageError::Message(format!(
                "deferred completion has invalid agent turn id `{agent_turn_id}`: {error}"
            ))
        })?,
        parent_session_id: parse_session_id_text(
            &parent_session_id,
            "deferred-completion parent session",
        )?,
        kind: match kind.as_str() {
            "completed_early" => DeferredAgentCompletionKind::CompletedEarly,
            "crash_failed" => DeferredAgentCompletionKind::CrashFailed,
            _ => {
                return Err(StorageError::Message(format!(
                    "deferred completion has unknown kind `{kind}`"
                )));
            }
        },
        state: match state.as_str() {
            "pending" => DeferredAgentCompletionState::Pending,
            "superseded" => DeferredAgentCompletionState::Superseded,
            "released" => DeferredAgentCompletionState::Released,
            "discarded" => DeferredAgentCompletionState::Discarded,
            _ => {
                return Err(StorageError::Message(format!(
                    "deferred completion has unknown state `{state}`"
                )));
            }
        },
        resolved_by_terminal_event_id: resolved_by_terminal_event_id
            .map(|event_id| {
                event_id.parse::<RuntimeEventId>().map_err(|error| {
                    StorageError::Message(format!(
                        "deferred completion has invalid resolver terminal id `{event_id}`: {error}"
                    ))
                })
            })
            .transpose()?,
    }))
}

fn has_unclaimed_agent_trigger_in_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM agent_mailbox_messages AS mailbox
                 WHERE mailbox.recipient_session_id = ?1
                   AND mailbox.state = 'pending'
                   AND mailbox.trigger_turn = 1
             )",
            params![session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )
        .map_err(StorageError::from)
}

fn has_pending_owner_resume_requests_in_connection(
    connection: &Connection,
    owner_session_id: SessionId,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM agent_owner_resume_requests
                 WHERE owner_session_id = ?1 AND state = 'pending'
             )",
            params![owner_session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )
        .map_err(StorageError::from)
}

fn list_owner_resume_requests_in_connection(
    connection: &Connection,
    owner_session_id: SessionId,
    state: &'static str,
) -> Result<Vec<OwnerResumeRequest>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT owner_session_id, source_session_id, source_history_item_id,
                state, claimed_turn_id, created_at_ms, updated_at_ms
         FROM agent_owner_resume_requests
         WHERE owner_session_id = ?1 AND state = ?2
         ORDER BY created_at_ms ASC, source_history_item_id ASC",
    )?;
    let rows = statement
        .query_map(params![owner_session_id.to_string(), state], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(
                owner_session_id,
                source_session_id,
                request_id,
                state,
                claimed_turn_id,
                created_at_ms,
                updated_at_ms,
            )| {
                Ok(OwnerResumeRequest {
                    request_id: request_id.parse::<OwnerResumeRequestId>().map_err(|error| {
                        StorageError::Message(format!(
                            "owner-resume request has invalid source history id `{request_id}`: {error}"
                        ))
                    })?,
                    owner_session_id: parse_session_id_text(
                        &owner_session_id,
                        "owner-resume owner",
                    )?,
                    source_session_id: parse_session_id_text(
                        &source_session_id,
                        "owner-resume source",
                    )?,
                    state: match state.as_str() {
                        "pending" => OwnerResumeRequestState::Pending,
                        "claimed" => OwnerResumeRequestState::Claimed,
                        "resolved" => OwnerResumeRequestState::Resolved,
                        "cancelled" => OwnerResumeRequestState::Cancelled,
                        _ => {
                            return Err(StorageError::Message(format!(
                                "owner-resume request has unknown state `{state}`"
                            )));
                        }
                    },
                    claimed_turn_id: claimed_turn_id
                        .map(|value| {
                            value.parse::<TurnId>().map_err(|error| {
                                StorageError::Message(format!(
                                    "owner-resume request has invalid claimed turn `{value}`: {error}"
                                ))
                            })
                        })
                        .transpose()?,
                    created_at_ms,
                    updated_at_ms,
                })
            },
        )
        .collect()
}

fn claim_pending_owner_resume_requests_in_transaction(
    transaction: &Transaction<'_>,
    owner_session_id: SessionId,
    turn_id: TurnId,
    expected_request_id: OwnerResumeRequestId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    if oldest_pending_owner_resume_request_id_in_connection(transaction, owner_session_id)?
        != Some(expected_request_id)
    {
        return Ok(0);
    }
    transaction
        .execute(
            "UPDATE agent_owner_resume_requests
             SET state = 'claimed',
                 claimed_turn_id = ?2,
                 claimed_at_ms = ?3,
                 resolved_at_ms = NULL,
                 updated_at_ms = MAX(updated_at_ms, ?3)
             WHERE owner_session_id = ?1
               AND state = 'pending'",
            params![owner_session_id.to_string(), turn_id.to_string(), now_ms],
        )
        .map_err(StorageError::from)
}

fn resolve_claimed_owner_resume_requests_in_transaction(
    transaction: &Transaction<'_>,
    owner_session_id: SessionId,
    turn_id: TurnId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    transaction
        .execute(
            "UPDATE agent_owner_resume_requests
             SET state = 'resolved',
                 resolved_at_ms = ?3,
                 updated_at_ms = MAX(updated_at_ms, ?3)
             WHERE owner_session_id = ?1
               AND state = 'claimed'
               AND claimed_turn_id = ?2",
            params![owner_session_id.to_string(), turn_id.to_string(), now_ms],
        )
        .map_err(StorageError::from)
}

fn repend_claimed_owner_resume_requests_in_transaction(
    transaction: &Transaction<'_>,
    owner_session_id: SessionId,
    turn_id: TurnId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    transaction
        .execute(
            "UPDATE agent_owner_resume_requests
             SET state = 'pending',
                 claimed_turn_id = NULL,
                 claimed_at_ms = NULL,
                 resolved_at_ms = NULL,
                 updated_at_ms = MAX(updated_at_ms, ?3)
             WHERE owner_session_id = ?1
               AND state = 'claimed'
               AND claimed_turn_id = ?2",
            params![owner_session_id.to_string(), turn_id.to_string(), now_ms],
        )
        .map_err(StorageError::from)
}

fn schedule_owner_resume_for_released_deferred_handoff_in_transaction(
    transaction: &Transaction<'_>,
    handoff: Option<&StoredAgentCompletionHandoff>,
    now_ms: i64,
) -> Result<bool, StorageError> {
    let Some(handoff) = handoff else {
        return Ok(false);
    };
    Ok(transaction.execute(
        "INSERT OR IGNORE INTO agent_owner_resume_requests (
             owner_session_id,
             source_session_id,
             source_history_item_id,
             state,
             claimed_turn_id,
             created_at_ms,
             updated_at_ms,
             claimed_at_ms,
             resolved_at_ms
         )
         SELECT ?1, ?1, ?2, 'pending', NULL, ?3, ?3, NULL, NULL
         WHERE EXISTS (
             SELECT 1
             FROM session_spawn_edges AS owner_edge
             INNER JOIN sessions AS owner
               ON owner.id = owner_edge.child_session_id
             WHERE owner_edge.child_session_id = ?1
               AND (
                   (
                       ?4
                       AND owner.status IN ('idle', 'completed', 'failed')
                   )
                   OR (
                       owner.status = 'failed'
                       AND EXISTS (
                           SELECT 1
                           FROM effective_agent_deferred_completions AS deferred
                           WHERE deferred.agent_session_id = owner.id
                             AND deferred.kind = 'crash_failed'
                             AND deferred.state IN ('pending', 'superseded')
                       )
                   )
               )
         )",
        params![
            handoff.parent_session_id.to_string(),
            handoff.history_item_id.to_string(),
            now_ms,
            handoff.released_owner_deferred_turn_id.is_some(),
        ],
    )? == 1)
}

fn seed_owner_resumes_for_released_deferred_handoffs_in_transaction(
    transaction: &Transaction<'_>,
    root_session_id: SessionId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO agent_owner_resume_requests (
                 owner_session_id,
                 source_session_id,
                 source_history_item_id,
                 state,
                 claimed_turn_id,
                 created_at_ms,
                 updated_at_ms,
                 claimed_at_ms,
                 resolved_at_ms
             )
             SELECT
                 handoff.parent_session_id,
                 handoff.parent_session_id,
                 handoff.parent_history_item_id,
                 'pending',
                 NULL,
                 MIN(handoff.created_at_ms, ?2),
                 ?2,
                 NULL,
                 NULL
             FROM agent_completion_handoffs AS handoff
             INNER JOIN agent_mailbox_messages AS mailbox
               ON mailbox.id = handoff.parent_history_item_id
              AND mailbox.recipient_session_id = handoff.parent_session_id
              AND mailbox.state = 'pending'
             INNER JOIN protocol_item_append_order AS result_order
               ON result_order.session_id = mailbox.recipient_session_id
              AND result_order.source_kind = 'mailbox_message'
              AND result_order.source_id = mailbox.id
             INNER JOIN session_spawn_edges AS owner_edge
               ON owner_edge.root_session_id = ?1
              AND owner_edge.child_session_id = handoff.parent_session_id
             INNER JOIN sessions AS owner
               ON owner.id = handoff.parent_session_id
             WHERE (
                    EXISTS (
                        SELECT 1
                        FROM effective_agent_deferred_completions AS released
                        WHERE released.agent_session_id = owner.id
                          AND released.state = 'superseded'
                          AND released.resolved_by_terminal_event_id =
                              handoff.child_terminal_event_id
                    )
                    OR (
                        owner.status = 'failed'
                        AND EXISTS (
                            SELECT 1
                            FROM effective_agent_deferred_completions AS deferred
                            WHERE deferred.agent_session_id = owner.id
                              AND deferred.kind = 'crash_failed'
                              AND deferred.state IN ('pending', 'superseded')
                        )
                    )
                )
               AND NOT EXISTS (
                   WITH RECURSIVE fenced_scope(
                       root_session_id,
                       stopped_session_id,
                       after_append_position,
                       session_id
                   ) AS (
                       SELECT
                           fence.root_session_id,
                           fence.stopped_session_id,
                           fence.after_append_position,
                           fence.stopped_session_id
                       FROM agent_tree_stop_fences AS fence
                       UNION ALL
                       SELECT
                           parent.root_session_id,
                           parent.stopped_session_id,
                           parent.after_append_position,
                           edge.child_session_id
                       FROM fenced_scope AS parent
                       INNER JOIN session_spawn_edges AS edge
                         ON edge.root_session_id = parent.root_session_id
                        AND edge.parent_session_id = parent.session_id
                   )
                   SELECT 1
                   FROM fenced_scope AS fence
                   WHERE fence.session_id = handoff.child_session_id
                     AND (
                         SELECT MIN(child_turn_order.append_position)
                         FROM protocol_item_append_order AS child_turn_order
                         WHERE child_turn_order.session_id =
                               handoff.child_session_id
                           AND child_turn_order.turn_id =
                               handoff.child_turn_id
                     ) <= fence.after_append_position
               )",
            params![root_session_id.to_string(), now_ms],
        )
        .map_err(StorageError::from)
}

fn validate_pending_agent_terminal(terminal: &DurableTurnTerminal) -> Result<(), StorageError> {
    if matches!(terminal.outcome, TurnTerminalOutcome::Completed) {
        return Err(StorageError::Message(
            "a pending agent trigger may only be settled as failed or interrupted".to_string(),
        ));
    }
    if terminal.final_response_id.is_some()
        || terminal.tool_call_count != 0
        || terminal.failed_tool_count != 0
        || terminal.change_count != 0
        || terminal.metrics.model_request_count != 0
        || terminal.metrics.elapsed_ms.is_some()
        || terminal.metrics.token_usage.is_some()
        || !terminal.metrics.tool_calls_by_name.is_empty()
        || !terminal.metrics.failed_tool_calls_by_name.is_empty()
        || terminal.metrics.config.is_some()
    {
        return Err(StorageError::Message(
            "a pre-admission agent terminal cannot claim responses, tools, changes, or run metrics"
                .to_string(),
        ));
    }
    Ok(())
}

fn settle_pending_agent_trigger_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    expected_history_item_id: HistoryItemId,
    now: i64,
    terminal: DurableTurnTerminal,
    tree_stop_origin: Option<ApplicableAgentTreeStopFence>,
) -> Result<PendingAgentTriggerSettlement, StorageError> {
    validate_pending_agent_terminal(&terminal)?;
    if let Some(tree_stop_origin) = tree_stop_origin
        && !terminal_is_compatible_with_tree_stop_fence(session_id, &terminal, tree_stop_origin)
    {
        return Err(StorageError::Message(format!(
            "synthetic tree-stop settlement for session {session_id} does not match the first durable Stop owner"
        )));
    }
    let Some(runtime_state) = session_runtime_state_from_connection(transaction, session_id)?
    else {
        return Err(StorageError::Message(format!(
            "pending agent trigger target session {session_id} does not exist"
        )));
    };
    if let Some(durable_admission) = runtime_state.admission {
        if durable_admission.is_fresh_at(now) {
            return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
        }
        recover_expired_run_admission_in_transaction(
            transaction,
            session_id,
            runtime_state.status,
            durable_admission,
            now,
        )?;
    }
    if !pending_agent_trigger_is_unclaimed_in_transaction(
        transaction,
        session_id,
        expected_history_item_id,
        tree_stop_origin.is_some(),
    )? {
        return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
    }
    let pending_deferred =
        deferred_agent_completion_in_connection(transaction, session_id, None, Some("pending"))?;
    let explicit_trigger_can_recover_crash = pending_deferred
        .as_ref()
        .is_some_and(|deferred| deferred.kind == DeferredAgentCompletionKind::CrashFailed);
    let stop_can_discard_pending_deferred =
        pending_deferred.as_ref().is_some_and(|deferred| {
            match (&deferred.kind, &terminal.outcome) {
                (
                    DeferredAgentCompletionKind::CompletedEarly,
                    TurnTerminalOutcome::Interrupted {
                        cause:
                            crate::protocol::TurnInterruptionCause::ApprovalAborted
                            | crate::protocol::TurnInterruptionCause::UserStop,
                    },
                ) => true,
                (
                    DeferredAgentCompletionKind::CompletedEarly,
                    TurnTerminalOutcome::Interrupted {
                        cause: crate::protocol::TurnInterruptionCause::TreeStopped,
                    },
                ) => tree_stop_origin.is_some(),
                (
                    DeferredAgentCompletionKind::CrashFailed,
                    TurnTerminalOutcome::Interrupted { cause },
                ) => {
                    !matches!(cause, crate::protocol::TurnInterruptionCause::TreeStopped)
                        || tree_stop_origin.is_some()
                }
                _ => false,
            }
        });
    if pending_deferred.is_some()
        && !explicit_trigger_can_recover_crash
        && !stop_can_discard_pending_deferred
    {
        return Ok(
            PendingAgentTriggerSettlement::BlockedByPendingDeferredCompletion {
                deferred_turn_id: pending_deferred
                    .expect("pending deferred owner was checked")
                    .agent_turn_id,
            },
        );
    }

    let turn_id = TurnId::new();
    let admission_id = AdmissionId::new();
    let lease_expires_at_ms = run_lease_expiry_ms(now, RUN_ADMISSION_LEASE_DURATION_MS);
    ensure_turn_identity_unused_in_transaction(transaction, session_id, turn_id)?;
    let admitted = transaction.execute(
        "UPDATE sessions
         SET status = 'running',
             updated_at_ms = ?2,
             completed_at_ms = NULL,
             active_run_id = ?3,
             active_turn_id = ?4,
             active_run_lease_expires_at_ms = ?5
         WHERE id = ?1
           AND active_run_id IS NULL
           AND status IN ('idle', 'completed', 'cancelled', 'failed')",
        params![
            session_id.to_string(),
            now,
            admission_id.to_string(),
            turn_id.to_string(),
            lease_expires_at_ms,
        ],
    )? == 1;
    if !admitted {
        return Err(StorageError::Message(format!(
            "pending agent trigger admission for session {session_id} lost its validated runtime-state owner"
        )));
    }

    let session_title = transaction.query_row(
        "SELECT title FROM sessions WHERE id = ?1",
        params![session_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    let started = RunEvent::SessionStarted {
        session_id,
        title: session_title,
    };
    let started_projection = project_protocol_run_event(&started, Some(session_id), turn_id, 0)
        .ok_or_else(|| {
            StorageError::Message("SessionStarted did not produce a protocol bundle".to_string())
        })?;
    insert_session_owned_event_bundle_in_transaction(
        &SESSION_PROTOCOL_WRITE_AUTHORITY,
        transaction,
        &started_projection.runtime_event,
        started_projection.history_item.as_ref(),
        started_projection.turn_item.as_ref(),
    )?;
    insert_agent_trigger_turn_claim_in_transaction(
        transaction,
        session_id,
        admission_id,
        turn_id,
        expected_history_item_id,
        now,
    )?;
    if let Some(pending_request_id) =
        oldest_pending_owner_resume_request_id_in_connection(transaction, session_id)?
    {
        claim_pending_owner_resume_requests_in_transaction(
            transaction,
            session_id,
            turn_id,
            pending_request_id,
            now,
        )?;
    }
    if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
        deliver_claimed_explicit_agent_wake_in_transaction(
            transaction,
            session_id,
            turn_id,
            expected_history_item_id,
            now,
        )?;
    }

    let terminal_status = terminal.session_status();
    let terminalized = transaction.execute(
        "UPDATE sessions
         SET status = ?5,
             updated_at_ms = ?6,
             completed_at_ms = ?6,
             active_run_id = NULL,
             active_turn_id = NULL,
             active_run_lease_expires_at_ms = NULL
         WHERE id = ?1
           AND active_run_id = ?2
           AND active_turn_id = ?3
           AND active_run_lease_expires_at_ms = ?4
           AND status = 'running'",
        params![
            session_id.to_string(),
            admission_id.to_string(),
            turn_id.to_string(),
            lease_expires_at_ms,
            session_status_text(terminal_status),
            now,
        ],
    )? == 1;
    if !terminalized {
        return Err(StorageError::Message(format!(
            "pending agent trigger settlement lost its admitted owner for session {session_id} turn {turn_id}"
        )));
    }
    let event = RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(terminal.clone()),
    };
    let terminal_sequence_no = settle_unfinished_tool_calls_for_terminal_event(
        transaction,
        session_id,
        &event,
        turn_id,
        1,
        now,
    )?;
    insert_protocol_projection_if_requested(
        transaction,
        &event,
        Some(session_id),
        turn_id,
        Some(terminal_sequence_no),
    )?;
    if tree_stop_origin.is_none()
        && matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. })
    {
        let resolver_terminal_event_id =
            exact_terminal_event_id_in_transaction(transaction, session_id, turn_id)?;
        discard_pending_explicit_agent_wake_in_transaction(
            transaction,
            session_id,
            expected_history_item_id,
            resolver_terminal_event_id,
            now,
        )?;
    }
    if explicit_trigger_can_recover_crash
        && matches!(terminal.outcome, TurnTerminalOutcome::Failed { .. })
    {
        let resolver_terminal_event_id =
            exact_terminal_event_id_in_transaction(transaction, session_id, turn_id)?;
        supersede_pending_deferred_completion_in_transaction(
            transaction,
            session_id,
            resolver_terminal_event_id,
            now,
        )?;
    }
    if stop_can_discard_pending_deferred && tree_stop_origin.is_none() {
        let resolver_terminal_event_id =
            exact_terminal_event_id_in_transaction(transaction, session_id, turn_id)?;
        discard_pending_deferred_completion_for_self_stop_in_transaction(
            transaction,
            session_id,
            resolver_terminal_event_id,
            now,
        )?;
    } else if explicit_trigger_can_recover_crash
        && matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. })
    {
        let resolver_terminal_event_id =
            exact_terminal_event_id_in_transaction(transaction, session_id, turn_id)?;
        discard_pending_crash_deferred_completion_in_transaction(
            transaction,
            session_id,
            resolver_terminal_event_id,
            now,
        )?;
    }
    resolve_claimed_owner_resume_requests_in_transaction(transaction, session_id, turn_id, now)?;
    let handoff = resolve_agent_completion_handoff_disposition_in_transaction(
        transaction,
        session_id,
        turn_id,
        append_agent_completion_handoff_in_transaction(
            transaction,
            session_id,
            turn_id,
            &terminal,
            now,
        )?,
        now,
    )?;
    if tree_stop_origin.is_some() {
        // The exact fence already discarded only its pre-boundary generations.
    } else if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
        release_quiescent_deferred_completions_after_interruption_in_transaction(
            transaction,
            session_id,
            turn_id,
            now,
        )?;
    }
    Ok(PendingAgentTriggerSettlement::Applied { turn_id, handoff })
}

fn settle_pending_owner_resume_in_transaction(
    transaction: &Transaction<'_>,
    owner_session_id: SessionId,
    expected_request_id: OwnerResumeRequestId,
    now: i64,
    terminal: DurableTurnTerminal,
) -> Result<PendingAgentTriggerSettlement, StorageError> {
    validate_pending_agent_terminal(&terminal)?;
    let Some(runtime_state) = session_runtime_state_from_connection(transaction, owner_session_id)?
    else {
        return Err(StorageError::Message(format!(
            "owner-resume target session {owner_session_id} does not exist"
        )));
    };
    if let Some(durable_admission) = runtime_state.admission {
        if durable_admission.is_fresh_at(now) {
            return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
        }
        recover_expired_run_admission_in_transaction(
            transaction,
            owner_session_id,
            runtime_state.status,
            durable_admission,
            now,
        )?;
    }
    if schedulable_owner_resume_request_id_in_connection(transaction, owner_session_id)?
        != Some(expected_request_id)
    {
        return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
    }

    let turn_id = TurnId::new();
    let admission_id = AdmissionId::new();
    let lease_expires_at_ms = run_lease_expiry_ms(now, RUN_ADMISSION_LEASE_DURATION_MS);
    ensure_turn_identity_unused_in_transaction(transaction, owner_session_id, turn_id)?;
    let admitted = transaction.execute(
        "UPDATE sessions
         SET status = 'running',
             updated_at_ms = ?2,
             completed_at_ms = NULL,
             active_run_id = ?3,
             active_turn_id = ?4,
             active_run_lease_expires_at_ms = ?5
         WHERE id = ?1
           AND active_run_id IS NULL
           AND status IN ('idle', 'completed', 'cancelled', 'failed')",
        params![
            owner_session_id.to_string(),
            now,
            admission_id.to_string(),
            turn_id.to_string(),
            lease_expires_at_ms,
        ],
    )? == 1;
    if !admitted {
        return Err(StorageError::Message(format!(
            "pending OwnerResume admission for session {owner_session_id} lost its validated runtime-state owner"
        )));
    }

    let session_title = transaction.query_row(
        "SELECT title FROM sessions WHERE id = ?1",
        params![owner_session_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    let started = RunEvent::SessionStarted {
        session_id: owner_session_id,
        title: session_title,
    };
    let started_projection =
        project_protocol_run_event(&started, Some(owner_session_id), turn_id, 0).ok_or_else(
            || {
                StorageError::Message(
                    "SessionStarted did not produce a protocol bundle".to_string(),
                )
            },
        )?;
    insert_session_owned_event_bundle_in_transaction(
        &SESSION_PROTOCOL_WRITE_AUTHORITY,
        transaction,
        &started_projection.runtime_event,
        started_projection.history_item.as_ref(),
        started_projection.turn_item.as_ref(),
    )?;
    if claim_pending_owner_resume_requests_in_transaction(
        transaction,
        owner_session_id,
        turn_id,
        expected_request_id,
        now,
    )? == 0
    {
        return Err(StorageError::Message(format!(
            "synthetic owner-resume settlement lost request {expected_request_id} for session {owner_session_id}"
        )));
    }

    let terminal_status = terminal.session_status();
    let terminalized = transaction.execute(
        "UPDATE sessions
         SET status = ?5,
             updated_at_ms = ?6,
             completed_at_ms = ?6,
             active_run_id = NULL,
             active_turn_id = NULL,
             active_run_lease_expires_at_ms = NULL
         WHERE id = ?1
           AND active_run_id = ?2
           AND active_turn_id = ?3
           AND active_run_lease_expires_at_ms = ?4
           AND status = 'running'",
        params![
            owner_session_id.to_string(),
            admission_id.to_string(),
            turn_id.to_string(),
            lease_expires_at_ms,
            session_status_text(terminal_status),
            now,
        ],
    )? == 1;
    if !terminalized {
        return Err(StorageError::Message(format!(
            "owner-resume settlement lost its admitted owner for session {owner_session_id} turn {turn_id}"
        )));
    }
    let event = RunEvent::TurnTerminal {
        session_id: owner_session_id,
        terminal: Box::new(terminal.clone()),
    };
    let terminal_sequence_no = settle_unfinished_tool_calls_for_terminal_event(
        transaction,
        owner_session_id,
        &event,
        turn_id,
        1,
        now,
    )?;
    insert_protocol_projection_if_requested(
        transaction,
        &event,
        Some(owner_session_id),
        turn_id,
        Some(terminal_sequence_no),
    )?;
    resolve_claimed_owner_resume_requests_in_transaction(
        transaction,
        owner_session_id,
        turn_id,
        now,
    )?;
    if matches!(terminal.outcome, TurnTerminalOutcome::Failed { .. }) {
        let resolver_terminal_event_id =
            exact_terminal_event_id_in_transaction(transaction, owner_session_id, turn_id)?;
        supersede_pending_deferred_completion_in_transaction(
            transaction,
            owner_session_id,
            resolver_terminal_event_id,
            now,
        )?;
    }
    if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
        let resolver_terminal_event_id =
            exact_terminal_event_id_in_transaction(transaction, owner_session_id, turn_id)?;
        discard_pending_crash_deferred_completion_in_transaction(
            transaction,
            owner_session_id,
            resolver_terminal_event_id,
            now,
        )?;
    }
    let handoff = resolve_agent_completion_handoff_disposition_in_transaction(
        transaction,
        owner_session_id,
        turn_id,
        append_agent_completion_handoff_in_transaction(
            transaction,
            owner_session_id,
            turn_id,
            &terminal,
            now,
        )?,
        now,
    )?;
    if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
        release_quiescent_deferred_completions_after_interruption_in_transaction(
            transaction,
            owner_session_id,
            turn_id,
            now,
        )?;
    }
    Ok(PendingAgentTriggerSettlement::Applied { turn_id, handoff })
}

fn recover_expired_run_admission_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    current_status: SessionStatus,
    recovery_admission: DurableRunAdmission,
    now_ms: i64,
) -> Result<(), StorageError> {
    let recovery_turn_id = recovery_admission.turn_id;
    let was_active = current_status == SessionStatus::Running;
    let recovering_owner_resume = if was_active {
        transaction.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM agent_owner_resume_requests
                 WHERE owner_session_id = ?1
                   AND state = 'claimed'
                   AND claimed_turn_id = ?2
             )",
            params![session_id.to_string(), recovery_turn_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?
    } else {
        false
    };
    let has_durable_descendant_work =
        was_active && session_has_durable_descendant_work_in_connection(transaction, session_id)?;
    let pending_direct_child_result = if was_active {
        pending_direct_child_result_terminal_in_connection(transaction, session_id)?
    } else {
        None
    };
    let retained_parent = retained_agent_parent_in_connection(transaction, session_id)?;
    validate_retained_admission_terminal_state_in_connection(
        transaction,
        session_id,
        ValidatedSessionRuntimeState {
            status: current_status,
            admission: Some(recovery_admission),
        },
    )?;
    let recovery_tree_stop_fence = if was_active {
        first_applicable_tree_stop_fence_for_turn_in_connection(
            transaction,
            session_id,
            recovery_turn_id,
        )?
    } else {
        None
    };
    let recovery_outcome = recovery_tree_stop_fence
        .map(|fence| recovery_terminal_outcome_for_tree_stop_fence(session_id, fence))
        .unwrap_or_else(|| TurnTerminalOutcome::Failed {
            error: EXPIRED_RUN_RECOVERY_REASON.to_string(),
        });
    if was_active {
        transaction.execute(
            "UPDATE sessions
             SET status = ?3,
                 updated_at_ms = ?2,
                 completed_at_ms = ?2,
                 active_run_id = NULL,
                 active_turn_id = NULL,
                 active_run_lease_expires_at_ms = NULL
             WHERE id = ?1",
            params![
                session_id.to_string(),
                now_ms,
                session_status_text(recovery_outcome.session_status()),
            ],
        )?;
    } else {
        transaction.execute(
            "UPDATE sessions
             SET updated_at_ms = MAX(updated_at_ms, ?2),
                 active_run_id = NULL,
                 active_turn_id = NULL,
                 active_run_lease_expires_at_ms = NULL
             WHERE id = ?1",
            params![session_id.to_string(), now_ms],
        )?;
    }
    if was_active {
        let turn_id = recovery_turn_id;
        let snapshot = canonical_turn_snapshot_in_transaction(transaction, session_id, turn_id)?;
        let recoverable_unfinished_count =
            count_unfinished_tool_calls_for_turn_in_transaction(transaction, session_id, turn_id)?;
        let event = RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                outcome: recovery_outcome,
                final_response_id: None,
                tool_call_count: snapshot.tool_call_count,
                failed_tool_count: snapshot
                    .failed_tool_count
                    .saturating_add(recoverable_unfinished_count),
                change_count: snapshot.change_count,
                metrics: Default::default(),
            }),
        };
        let recovery_sequence_no = resolve_terminal_protocol_sequence_in_transaction(
            transaction,
            session_id,
            turn_id,
            None,
        )?;
        let terminal_sequence_no = settle_unfinished_tool_calls_for_terminal_event(
            transaction,
            session_id,
            &event,
            turn_id,
            recovery_sequence_no,
            now_ms,
        )?;
        insert_protocol_projection_if_requested(
            transaction,
            &event,
            Some(session_id),
            turn_id,
            Some(terminal_sequence_no),
        )?;
        let recovery_turn_is_tree_stop_fenced = recovery_tree_stop_fence.is_some();
        if recovery_turn_is_tree_stop_fenced {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(transaction, session_id, turn_id)?;
            discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
                transaction,
                session_id,
                resolver_terminal_event_id,
                now_ms,
            )?;
        }
        if recovering_owner_resume && !recovery_turn_is_tree_stop_fenced {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(transaction, session_id, turn_id)?;
            supersede_pending_deferred_completion_in_transaction(
                transaction,
                session_id,
                resolver_terminal_event_id,
                now_ms,
            )?;
        }
        let repended_owner_resume = if recovering_owner_resume {
            repend_claimed_owner_resume_requests_in_transaction(
                transaction,
                session_id,
                recovery_turn_id,
                now_ms,
            )?;
            true
        } else {
            has_pending_owner_resume_requests_in_connection(transaction, session_id)?
        };
        if recovery_turn_is_tree_stop_fenced {
            // A recovered lease cannot publish or defer work from a generation that a durable
            // tree Stop already closed.
        } else if let Some(parent_session_id) = retained_parent.filter(|_| {
            has_durable_descendant_work
                || pending_direct_child_result.is_some()
                || repended_owner_resume
        }) {
            insert_deferred_agent_completion_in_transaction(
                transaction,
                session_id,
                turn_id,
                parent_session_id,
                DeferredAgentCompletionKind::CrashFailed,
                now_ms,
            )?;
            if let Some(resolver_terminal_event_id) = pending_direct_child_result {
                supersede_pending_deferred_completion_in_transaction(
                    transaction,
                    session_id,
                    resolver_terminal_event_id,
                    now_ms,
                )?;
                let root_session_id = transaction.query_row(
                    "SELECT root_session_id
                     FROM session_spawn_edges
                     WHERE child_session_id = ?1",
                    params![session_id.to_string()],
                    |row| row.get::<_, String>(0),
                )?;
                seed_owner_resumes_for_released_deferred_handoffs_in_transaction(
                    transaction,
                    parse_session_id_text(&root_session_id, "expired deferred owner tree root")?,
                    now_ms,
                )?;
            }
        } else if !repended_owner_resume {
            append_agent_completion_handoff_in_transaction(
                transaction,
                session_id,
                turn_id,
                validate_terminal_event(session_id, &event)?,
                now_ms,
            )?;
        }
    }
    // A terminal session already settled the tools owned by this turn. Expiry only releases
    // the stale admission; it must not reclassify first-writer terminal outcomes.
    Ok(())
}

pub(crate) fn fresh_active_admission_matches_in_connection(
    connection: &Connection,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    now_ms: i64,
) -> Result<bool, StorageError> {
    let now = normalize_run_lease_now_ms(now_ms);
    let owned = session_runtime_state_from_connection(connection, session_id)?
        .filter(|runtime_state| runtime_state.status == SessionStatus::Running)
        .and_then(|runtime_state| runtime_state.fresh_admission_at(now))
        .is_some_and(|admission| {
            admission.admission_id == admission_id && admission.turn_id == turn_id
        });
    if !owned {
        return Ok(false);
    }
    Ok(
        !turn_started_before_applicable_tree_stop_fence_in_transaction(
            connection, session_id, turn_id,
        )?,
    )
}

fn require_active_admission_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
) -> Result<(), StorageError> {
    let owned = fresh_active_admission_matches_in_connection(
        transaction,
        session_id,
        admission_id,
        turn_id,
        SystemClock::now_ms(),
    )?;
    if owned {
        Ok(())
    } else {
        Err(StorageError::Message(format!(
            "run admission {admission_id} no longer owns active turn {turn_id} for session {session_id}"
        )))
    }
}

fn count_unfinished_tool_calls_for_turn_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<usize, StorageError> {
    let count = transaction.query_row(
        "SELECT COUNT(*)
         FROM tool_calls AS tool
         INNER JOIN protocol_history_items AS history
            ON history.id = tool.history_item_id
         WHERE history.session_id = ?1
           AND history.turn_id = ?2
           AND tool.status IN ('pending', 'running')",
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count as usize)
}

fn resolve_terminal_protocol_sequence_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    protocol_turn_id: TurnId,
    requested_sequence_no: Option<i64>,
) -> Result<i64, StorageError> {
    if let Some(sequence_no) = requested_sequence_no {
        return Ok(sequence_no);
    }
    let max_sequence_no = transaction.query_row(
        "SELECT MAX(sequence_no)
         FROM (
           SELECT sequence_no
           FROM protocol_runtime_events
           WHERE session_id = ?1 AND turn_id = ?2
           UNION ALL
           SELECT sequence_no
           FROM protocol_history_items
           WHERE session_id = ?1 AND turn_id = ?2
           UNION ALL
           SELECT sequence_no
           FROM protocol_turn_items
           WHERE session_id = ?1 AND turn_id = ?2
         )",
        params![session_id.to_string(), protocol_turn_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(max_sequence_no.unwrap_or(-1).saturating_add(1))
}

fn settle_unfinished_tool_calls_for_terminal_event(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    event: &RunEvent,
    protocol_turn_id: TurnId,
    protocol_sequence_no: i64,
    finished_at_ms: i64,
) -> Result<i64, StorageError> {
    let terminal = validate_terminal_event(session_id, event)?;
    let (status, reason) = match &terminal.outcome {
        TurnTerminalOutcome::Interrupted { .. } => (
            ToolCallStatus::Cancelled,
            if terminal.summary().trim().is_empty() {
                "turn interrupted before the tool call finished"
            } else {
                terminal.summary()
            },
        ),
        TurnTerminalOutcome::Failed { .. } => (
            ToolCallStatus::Failed,
            if terminal.summary().trim().is_empty() {
                "turn failed before the tool call finished"
            } else {
                terminal.summary()
            },
        ),
        TurnTerminalOutcome::Completed => (
            ToolCallStatus::Cancelled,
            "turn completed before the tool call finished",
        ),
    };

    let unfinished = {
        let mut statement = transaction.prepare(
            "SELECT tool.id, history.payload_json
             FROM tool_calls AS tool
             INNER JOIN protocol_history_items AS history
                ON history.id = tool.history_item_id
             WHERE history.session_id = ?1
               AND history.turn_id = ?2
               AND tool.status IN ('pending', 'running')
             ORDER BY tool.started_at_ms ASC, tool.id ASC",
        )?;
        statement
            .query_map(
                params![session_id.to_string(), protocol_turn_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut next_sequence_no = protocol_sequence_no;
    for (tool_call_id, payload_json) in unfinished {
        let tool_call_id = tool_call_id.parse::<ToolCallId>().map_err(|error| {
            StorageError::Message(format!("invalid durable tool call id: {error}"))
        })?;
        let payload = serde_json::from_str::<HistoryItemPayload>(&payload_json)?;
        let HistoryItemPayload::ToolCall {
            call_id, tool_name, ..
        } = payload
        else {
            return Err(StorageError::Message(format!(
                "tool sidecar {tool_call_id} does not reference a canonical ToolCall item"
            )));
        };
        if call_id != tool_call_id {
            return Err(StorageError::Message(format!(
                "tool sidecar id {tool_call_id} contradicts canonical call id {call_id}"
            )));
        }
        let tool = crate::tool::ToolName::parse(&tool_name);
        let applied = match status {
            ToolCallStatus::Cancelled => transaction.execute(
                "UPDATE tool_calls
                 SET status = 'cancelled', finished_at_ms = ?2
                 WHERE id = ?1
                   AND history_item_id IN (
                       SELECT id FROM protocol_history_items
                       WHERE session_id = ?3 AND turn_id = ?4
                   )
                   AND status IN ('pending', 'running')",
                params![
                    tool_call_id.to_string(),
                    finished_at_ms,
                    session_id.to_string(),
                    protocol_turn_id.to_string(),
                ],
            )?,
            ToolCallStatus::Failed => transaction.execute(
                "UPDATE tool_calls
                 SET status = 'failed', finished_at_ms = ?2
                 WHERE id = ?1
                   AND history_item_id IN (
                       SELECT id FROM protocol_history_items
                       WHERE session_id = ?3 AND turn_id = ?4
                   )
                   AND status IN ('pending', 'running')",
                params![
                    tool_call_id.to_string(),
                    finished_at_ms,
                    session_id.to_string(),
                    protocol_turn_id.to_string(),
                ],
            )?,
            _ => unreachable!("terminal sweep only cancels or fails unfinished tools"),
        } == 1;
        if !applied {
            continue;
        }
        let tool_event = match status {
            ToolCallStatus::Cancelled => RunEvent::ToolCallCancelled {
                tool_call_id,
                tool,
                reason: reason.to_string(),
                metadata: serde_json::Value::Null,
            },
            ToolCallStatus::Failed => RunEvent::ToolCallFailed {
                tool_call_id,
                tool,
                error: reason.to_string(),
                metadata: serde_json::Value::Null,
            },
            _ => unreachable!("terminal sweep only cancels or fails unfinished tools"),
        };
        insert_protocol_projection_if_requested(
            transaction,
            &tool_event,
            Some(session_id),
            protocol_turn_id,
            Some(next_sequence_no),
        )?;
        next_sequence_no = next_sequence_no.saturating_add(1);
    }
    Ok(next_sequence_no)
}

fn escape_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::config::AccessMode;
    use crate::protocol::{
        ContentPart, InterAgentCommunication, ModeKind, ProtocolEventStore, ToolLifecycleStatus,
        UserInputItem,
    };
    use crate::session::{ChangeId, ChangeKind, ChangeRepository, NewSession, ProjectRepository};
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    async fn test_repo() -> (StoreBundle, SessionId) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 path");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "test", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "test".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::Default,
            })
            .await
            .expect("session");
        (store, session.id)
    }

    async fn create_sibling_session(
        store: &StoreBundle,
        root_session_id: SessionId,
        title: &str,
    ) -> SessionRecord {
        let draft = sibling_session_draft(store, root_session_id, title).await;
        store
            .session_repo()
            .create_session(draft)
            .await
            .expect("sibling session")
    }

    async fn sibling_session_draft(
        store: &StoreBundle,
        root_session_id: SessionId,
        title: &str,
    ) -> NewSession {
        let root = store
            .session_repo()
            .get_session(root_session_id)
            .await
            .expect("root session");
        NewSession {
            project_id: root.project_id,
            title: title.to_string(),
            cwd: root.cwd,
            model: root.model,
            base_url: root.base_url,
            access_mode: root.access_mode,
        }
    }

    async fn spawn_pending_child(
        store: &StoreBundle,
        root_session_id: SessionId,
        task_name: &str,
    ) -> (SessionRecord, HistoryItemId, TurnId) {
        let root_turn_id = TurnId::new();
        let root_admission = store
            .session_repo()
            .admit_session_turn(root_session_id, root_turn_id)
            .await
            .expect("root admission")
            .expect("root admitted");
        let child_session_id = SessionId::new();
        let child_draft = sibling_session_draft(store, root_session_id, task_name).await;
        let child_path = format!("/root/{task_name}");
        let initial_task = InterAgentCommunication {
            author: "/root".to_string(),
            recipient: child_path.clone(),
            content: render_inter_agent_message(
                InterAgentMessageType::NewTask,
                &child_path,
                "/root",
                "run the bounded task",
            ),
            trigger_turn: true,
        };
        let stored = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                child_session_id,
                child_draft,
                &child_path,
                task_name,
                root_admission.admission_id,
                root_turn_id,
                SpawnContextFork::None,
                initial_task,
            )
            .expect("atomic pending child spawn");
        (
            stored.child_session,
            stored.initial_task_history_item_id,
            root_turn_id,
        )
    }

    fn pre_admission_failed_terminal(error: &str) -> DurableTurnTerminal {
        DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Failed {
                error: error.to_string(),
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }
    }

    fn pre_admission_interrupted_terminal() -> DurableTurnTerminal {
        DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::TreeStopped,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }
    }

    fn pre_admission_user_stopped_terminal() -> DurableTurnTerminal {
        DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }
    }

    fn pre_admission_agent_interrupted_terminal() -> DurableTurnTerminal {
        DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::AgentInterrupted,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }
    }

    async fn nested_owner_resume_fixture(
        store: &StoreBundle,
        root_session_id: SessionId,
        child_count: usize,
    ) -> (SessionRecord, Vec<OwnerResumeRequest>) {
        let repository = store.session_repo();
        let owner = create_sibling_session(store, root_session_id, "owner").await;
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                owner.id,
                "/root/owner",
                "owner",
            )
            .await
            .expect("owner edge");
        for index in 0..child_count {
            let task_name = format!("child_{index}");
            let agent_path = format!("/root/owner/{task_name}");
            let child = create_sibling_session(store, root_session_id, &task_name).await;
            repository
                .insert_session_spawn_edge(
                    root_session_id,
                    owner.id,
                    child.id,
                    &agent_path,
                    &task_name,
                )
                .await
                .expect("nested child edge");
            let (admission_id, turn_id) = active_turn(store, child.id).await;
            assert_eq!(
                repository
                    .terminalize_admitted_turn_with_protocol_event(
                        child.id,
                        admission_id,
                        &failed_terminal(child.id, "child failed"),
                        turn_id,
                        None,
                        None,
                    )
                    .await
                    .expect("child terminal"),
                AdmittedTerminalCommit::Applied
            );
            // This fixture exercises the OwnerResume state machine itself. Current Codex-aligned
            // completion delivery leaves an idle owner dormant unless a previously deferred
            // owner generation was released, so seed the otherwise-valid durable wake explicitly
            // from the direct-child FINAL handoff.
            let handoff = repository
                .agent_completion_handoff(child.id, turn_id)
                .expect("child completion handoff")
                .expect("stored child completion handoff");
            let now = normalize_run_lease_now_ms(SystemClock::now_ms());
            let connection = repository.connection.lock().expect("sqlite mutex");
            connection
                .execute(
                    "INSERT OR IGNORE INTO agent_owner_resume_requests (
                         owner_session_id,
                         source_session_id,
                         source_history_item_id,
                         state,
                         claimed_turn_id,
                         created_at_ms,
                         updated_at_ms,
                         claimed_at_ms,
                         resolved_at_ms
                     )
                     VALUES (?1, ?1, ?2, 'pending', NULL, ?3, ?3, NULL, NULL)",
                    params![
                        owner.id.to_string(),
                        handoff.history_item_id.to_string(),
                        now
                    ],
                )
                .expect("seed valid OwnerResume wake");
        }
        let requests = repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("owner resume requests");
        assert_eq!(requests.len(), child_count);
        (owner, requests)
    }

    async fn retained_test_agent(
        store: &StoreBundle,
        root_session_id: SessionId,
        parent_session_id: SessionId,
        parent_path: &str,
        task_name: &str,
    ) -> SessionRecord {
        let session = create_sibling_session(store, root_session_id, task_name).await;
        let path = format!("{parent_path}/{task_name}");
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                parent_session_id,
                session.id,
                &path,
                task_name,
            )
            .await
            .expect("retained test-agent edge");
        session
    }

    fn agent_interrupted_terminal(session_id: SessionId) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::AgentInterrupted,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    fn tree_stopped_terminal(session_id: SessionId) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::TreeStopped,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    async fn orphan_crash_explicit_fixture(
        task_name: &str,
    ) -> (StoreBundle, SessionRecord, TurnId, StoredAgentCommunication) {
        let (store, root_session_id) = test_repo().await;
        let owner =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", task_name).await;
        let owner_path = format!("/root/{task_name}");
        let child =
            retained_test_agent(&store, root_session_id, owner.id, &owner_path, "child").await;
        let (_owner_admission, crashed_turn) = active_turn(&store, owner.id).await;
        let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
        let repository = store.session_repo();
        let target = repository
            .captured_running_terminal_target(owner.id)
            .await
            .expect("capture orphan crash owner")
            .expect("running orphan crash owner");
        assert!(
            repository
                .recover_captured_running_session_with_protocol_event(
                    owner.id,
                    &failed_terminal(owner.id, "owner process crashed"),
                    target,
                )
                .await
                .expect("recover orphan crash owner")
        );
        let explicit = repository
            .append_inter_agent_communication_with_protocol_bundle(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: owner_path.clone(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        &format!("/root/{task_name}"),
                        "/root",
                        "recover orphan crash",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("orphan crash explicit trigger");
        assert!(explicit.schedule_turn);
        (store, owner, crashed_turn, explicit)
    }

    #[tokio::test]
    async fn root_completed_terminal_is_independent_of_live_descendants() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (child_admission, child_turn) = active_turn(&store, child.id).await;
        let (root_admission, root_turn) = active_turn(&store, root_session_id).await;
        let repository = store.session_repo();

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    root_admission,
                    &completed_terminal_for_response(root_session_id, None),
                    root_turn,
                    None,
                    None,
                )
                .await
                .expect("root terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(matches!(
            repository
                .durable_terminal_for_turn(root_session_id, root_turn)
                .await
                .expect("root durable terminal")
                .expect("root terminal evidence")
                .outcome,
            TurnTerminalOutcome::Completed
        ));
        assert_eq!(
            repository
                .get_session(root_session_id)
                .await
                .expect("root session")
                .status,
            SessionStatus::Completed
        );
        assert_eq!(
            repository
                .get_session(child.id)
                .await
                .expect("live child")
                .status,
            SessionStatus::Running,
            "root success must not cancel or terminalize an active child"
        );

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission,
                    &completed_terminal_for_response(child.id, None),
                    child_turn,
                    None,
                    None,
                )
                .await
                .expect("late child terminal"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = repository
            .agent_completion_handoff(child.id, child_turn)
            .expect("late child handoff")
            .expect("late child result remains attached to its exact direct parent");
        assert_eq!(handoff.parent_session_id, root_session_id);
        assert!(matches!(
            repository
                .durable_terminal_for_turn(root_session_id, root_turn)
                .await
                .expect("root terminal reread")
                .expect("root terminal remains")
                .outcome,
            TurnTerminalOutcome::Completed
        ));
        assert!(
            repository
                .schedulable_owner_resume_request_id(root_session_id)
                .expect("root owner-resume lookup")
                .is_none(),
            "a late child result must not rewrite or reopen the terminal root"
        );
    }

    #[tokio::test]
    async fn nonroot_completed_terminal_handoffs_immediately_and_keeps_grandchild_live() {
        let (store, root_session_id) = test_repo().await;
        let owner =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
        let grandchild =
            retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "slow").await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (grandchild_admission, grandchild_turn) = active_turn(&store, grandchild.id).await;
        let repository = store.session_repo();

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("owner terminal"),
            AdmittedTerminalCommit::Applied
        );
        let owner_effects = repository
            .agent_terminal_effects(owner.id, owner_turn)
            .expect("owner terminal effects");
        assert!(
            owner_effects.deferred.is_none(),
            "normal completion no longer creates a descendant-liveness receipt"
        );
        let owner_handoff = owner_effects
            .completion_handoff
            .expect("owner result must reach its exact direct parent immediately");
        assert_eq!(owner_handoff.parent_session_id, root_session_id);
        assert_eq!(
            repository
                .get_session(grandchild.id)
                .await
                .expect("slow grandchild")
                .status,
            SessionStatus::Running
        );

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    grandchild.id,
                    grandchild_admission,
                    &completed_terminal_for_response(grandchild.id, None),
                    grandchild_turn,
                    None,
                    None,
                )
                .await
                .expect("late grandchild terminal"),
            AdmittedTerminalCommit::Applied
        );
        let late_handoff = repository
            .agent_completion_handoff(grandchild.id, grandchild_turn)
            .expect("late grandchild handoff")
            .expect("late result remains durably attached to the completed direct parent");
        assert_eq!(late_handoff.parent_session_id, owner.id);
        assert!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("completed owner resume lookup")
                .is_none(),
            "queue-only FINAL must not reopen a completed parent"
        );
        assert!(matches!(
            repository
                .durable_terminal_for_turn(owner.id, owner_turn)
                .await
                .expect("owner terminal reread")
                .expect("owner terminal remains")
                .outcome,
            TurnTerminalOutcome::Completed
        ));
    }

    #[tokio::test]
    async fn agent_interrupted_parent_retains_late_child_final_without_owner_resume() {
        let (store, root_session_id) = test_repo().await;
        let owner =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
        let child =
            retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (child_admission, child_turn) = active_turn(&store, child.id).await;
        let repository = store.session_repo();

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &agent_interrupted_terminal(owner.id),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("interrupt reusable owner"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission,
                    &completed_terminal_for_response(child.id, None),
                    child_turn,
                    None,
                    None,
                )
                .await
                .expect("late child result"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = repository
            .agent_completion_handoff(child.id, child_turn)
            .expect("late child handoff")
            .expect("AgentInterrupted parent retains child FINAL");
        assert_eq!(handoff.parent_session_id, owner.id);
        assert!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("owner resume lookup")
                .is_none(),
            "AgentInterrupted requires an explicit follow-up, not an automatic resume"
        );

        let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen interrupted owner");
        reopened_sqlite.migrate().expect("migrate reopened store");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert!(
            reopened
                .session_repo()
                .agent_completion_handoff(child.id, child_turn)
                .expect("reopened late child handoff")
                .is_some()
        );
        assert!(
            reopened
                .session_repo()
                .schedulable_owner_resume_request_id(owner.id)
                .expect("reopened owner resume lookup")
                .is_none()
        );
    }

    #[tokio::test]
    async fn agent_interruption_releases_deferred_final_to_cancelled_parent_without_rollback() {
        let (store, root_session_id) = test_repo().await;
        let parent =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "parent").await;
        let owner =
            retained_test_agent(&store, root_session_id, parent.id, "/root/parent", "owner").await;
        let leaf = retained_test_agent(
            &store,
            root_session_id,
            owner.id,
            "/root/parent/owner",
            "leaf",
        )
        .await;
        let (parent_admission, parent_turn) = active_turn(&store, parent.id).await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
        let repository = store.session_repo();

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    parent.id,
                    parent_admission,
                    &agent_interrupted_terminal(parent.id),
                    parent_turn,
                    None,
                    None,
                )
                .await
                .expect("interrupt reusable parent"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("defer early owner completion"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store, owner.id, owner_turn, parent.id,
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.id,
                    leaf_admission,
                    &agent_interrupted_terminal(leaf.id),
                    leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("leaf AgentInterrupted must commit"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, owner_turn)
                .expect("released owner effects")
                .deferred
                .expect("owner deferred receipt")
                .state,
            DeferredAgentCompletionState::Released
        );
        let _handoff = repository
            .agent_completion_handoff(owner.id, owner_turn)
            .expect("released owner handoff")
            .expect("forensic FINAL to cancelled parent");
        assert!(
            repository
                .schedulable_owner_resume_request_id(parent.id)
                .expect("cancelled parent owner resume")
                .is_none()
        );
        assert!(
            repository
                .durable_terminal_for_turn(leaf.id, leaf_turn)
                .await
                .expect("leaf durable terminal")
                .is_some(),
            "deferred release must not roll back the resolver terminal"
        );
    }

    #[tokio::test]
    async fn tree_stop_before_late_nested_result_discards_deferred_and_survives_restart() {
        let (store, root_session_id) = test_repo().await;
        let middle =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "middle").await;
        let leaf =
            retained_test_agent(&store, root_session_id, middle.id, "/root/middle", "leaf").await;
        let (root_admission, root_turn) = active_turn(&store, root_session_id).await;
        let (middle_admission, middle_turn) = active_turn(&store, middle.id).await;
        let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
        let repository = store.session_repo();

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    middle.id,
                    middle_admission,
                    &completed_terminal_for_response(middle.id, None),
                    middle_turn,
                    None,
                    None,
                )
                .await
                .expect("middle early completion"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            middle.id,
            middle_turn,
            root_session_id,
        );
        let root_stop = RunEvent::TurnTerminal {
            session_id: root_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert!(
            repository
                .record_agent_tree_stop_fence(
                    root_session_id,
                    crate::protocol::TurnInterruptionCause::UserStop,
                )
                .await
                .expect("record explicit root tree-Stop boundary")
                .is_some()
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    root_admission,
                    &root_stop,
                    root_turn,
                    None,
                    None,
                )
                .await
                .expect("durable root UserStop"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.id,
                    leaf_admission,
                    &completed_terminal_for_response(leaf.id, None),
                    leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("late leaf completion"),
            AdmittedTerminalCommit::NotOwned
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.id,
                    leaf_admission,
                    &tree_stopped_terminal(leaf.id),
                    leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("compatible stopped leaf terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            repository
                .agent_completion_handoff(leaf.id, leaf_turn)
                .expect("late leaf handoff lookup")
                .is_none()
        );
        assert_eq!(
            repository
                .agent_terminal_effects(middle.id, middle_turn)
                .expect("middle stopped effects")
                .deferred
                .expect("middle deferred receipt")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            repository
                .schedulable_owner_resume_request_id(middle.id)
                .expect("middle owner resume")
                .is_none()
        );

        let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen stopped tree");
        reopened_sqlite
            .migrate()
            .expect("migrate reopened stopped tree");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert!(
            reopened
                .session_repo()
                .schedulable_owner_resume_request_id(middle.id)
                .expect("reopened middle owner resume")
                .is_none()
        );
        reopened
            .session_repo()
            .delete_session_tree(root_session_id)
            .await
            .expect("durable stop fences must cascade with deleted tree");
    }

    #[tokio::test]
    async fn result_before_stop_keeps_forensic_handoff_but_restart_cannot_reseed_resume() {
        let (store, root_session_id) = test_repo().await;
        let owner =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
        let leaf =
            retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "leaf").await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
        let repository = store.session_repo();

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("owner early completion"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.id,
                    leaf_admission,
                    &completed_terminal_for_response(leaf.id, None),
                    leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("leaf result before Stop"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            repository
                .agent_completion_handoff(leaf.id, leaf_turn)
                .expect("forensic leaf handoff")
                .is_some()
        );
        assert!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("pre-Stop owner resume")
                .is_none()
        );
        assert!(
            repository
                .record_agent_tree_stop_fence(
                    root_session_id,
                    crate::protocol::TurnInterruptionCause::UserStop,
                )
                .await
                .expect("record result-after-Stop boundary")
                .is_some()
        );
        assert!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("post-Stop owner resume")
                .is_none()
        );

        let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen result-first tree");
        reopened_sqlite
            .migrate()
            .expect("migrate reopened result-first tree");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert!(
            reopened
                .session_repo()
                .agent_completion_handoff(leaf.id, leaf_turn)
                .expect("reopened forensic handoff")
                .is_some()
        );
        assert!(
            reopened
                .session_repo()
                .schedulable_owner_resume_request_id(owner.id)
                .expect("reopened owner resume")
                .is_none(),
            "normal completion must not seed an OwnerResume before or after Stop"
        );
    }

    #[tokio::test]
    async fn tree_stop_fence_revokes_old_generation_writes_and_preserves_first_cause() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (old_admission_id, old_turn_id) = active_turn(&store, root_session_id).await;
        let replacement_item_id = store
            .protocol_event_store()
            .list_history_items(root_session_id, old_turn_id)
            .expect("old turn history")
            .into_iter()
            .find(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
            .expect("old user turn")
            .id;
        let protocol_before = (
            store
                .protocol_event_store()
                .list_history_items(root_session_id, old_turn_id)
                .expect("history before fence")
                .len(),
            store
                .protocol_event_store()
                .list_runtime_events(root_session_id, old_turn_id)
                .expect("runtime before fence")
                .len(),
            store
                .protocol_event_store()
                .list_turn_items(root_session_id, old_turn_id)
                .expect("turn items before fence")
                .len(),
        );

        repository
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("record first Stop owner")
            .expect("first Stop fence");
        assert!(matches!(
            repository
                .renew_admitted_run_lease(root_session_id, old_admission_id, old_turn_id)
                .await
                .expect("old lease renewal"),
            RunAdmissionLeaseRenewalOutcome::StopFenced(TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop
            })
        ));
        assert!(matches!(
            repository
                .admitted_run_state(root_session_id, old_admission_id, old_turn_id)
                .await
                .expect("typed old admission state"),
            AdmittedRunState::StopFenced(TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop
            })
        ));
        assert_eq!(
            repository
                .admitted_run_status(root_session_id, old_admission_id, old_turn_id)
                .await
                .expect("old admitted status"),
            None
        );
        assert!(
            !repository
                .has_fresh_run_admission(root_session_id)
                .await
                .expect("old effective admission")
        );
        assert!(
            !repository
                .session_blocks_mutation(root_session_id)
                .await
                .expect("old effective mutation owner")
        );
        repository
            .record_model_response_with_protocol_bundle(
                root_session_id,
                old_admission_id,
                old_turn_id,
                ModelResponseWrite {
                    response_id: ModelResponseId::new(),
                    assistant_text: Some("late response must not commit".to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: Vec::new(),
                },
            )
            .await
            .expect_err("tree Stop must reject a late model response");
        repository
            .commit_admitted_compaction_with_protocol_bundle(
                root_session_id,
                old_admission_id,
                &RunEvent::CompactionCompleted {
                    summarized_messages: 1,
                    preserved_user_messages: vec!["canonical request".to_string()],
                    summary: "late compaction must not commit".to_string(),
                    replacement_item_ids: vec![replacement_item_id],
                },
                old_turn_id,
                None,
            )
            .await
            .expect_err("tree Stop must reject a late compaction");
        assert_eq!(
            (
                store
                    .protocol_event_store()
                    .list_history_items(root_session_id, old_turn_id)
                    .expect("history after rejected writes")
                    .len(),
                store
                    .protocol_event_store()
                    .list_runtime_events(root_session_id, old_turn_id)
                    .expect("runtime after rejected writes")
                    .len(),
                store
                    .protocol_event_store()
                    .list_turn_items(root_session_id, old_turn_id)
                    .expect("turn items after rejected writes")
                    .len(),
            ),
            protocol_before
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    old_admission_id,
                    &completed_terminal(root_session_id),
                    old_turn_id,
                    None,
                    None,
                )
                .await
                .expect("late success is a typed stale terminal"),
            AdmittedTerminalCommit::NotOwned
        );

        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/child",
                        "/root",
                        "append after first fence",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("advance durable append order");
        let later_fence = repository
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::ApprovalAborted,
            )
            .await
            .expect("record overlapping Stop")
            .expect("later overlapping fence");
        let old_target = repository
            .captured_running_terminal_target(root_session_id)
            .await
            .expect("capture overlapping target")
            .expect("old generation remains physically running before fanout");
        assert_eq!(
            repository
                .tree_stop_interruption_cause_for_running_target_at_fence(
                    root_session_id,
                    old_target,
                    later_fence,
                )
                .await
                .expect("derive first Stop cause"),
            Some(crate::protocol::TurnInterruptionCause::UserStop)
        );
        let approval_terminal = RunEvent::TurnTerminal {
            session_id: root_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::ApprovalAborted,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    old_admission_id,
                    &approval_terminal,
                    old_turn_id,
                    None,
                    None,
                )
                .await
                .expect("later Stop cause must not win"),
            AdmittedTerminalCommit::NotOwned
        );
        let user_stop_terminal = RunEvent::TurnTerminal {
            session_id: root_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    old_admission_id,
                    &user_stop_terminal,
                    old_turn_id,
                    None,
                    None,
                )
                .await
                .expect("first Stop cause closes old generation"),
            AdmittedTerminalCommit::Applied
        );

        let (new_admission_id, new_turn_id) = active_turn(&store, root_session_id).await;
        assert!(matches!(
            repository
                .renew_admitted_run_lease(root_session_id, new_admission_id, new_turn_id)
                .await
                .expect("new generation renewal"),
            RunAdmissionLeaseRenewalOutcome::Renewed
        ));
        repository
            .record_model_response_with_protocol_bundle(
                root_session_id,
                new_admission_id,
                new_turn_id,
                ModelResponseWrite {
                    response_id: ModelResponseId::new(),
                    assistant_text: Some("new generation response".to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: Vec::new(),
                },
            )
            .await
            .expect("new generation model response");
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    new_admission_id,
                    &completed_terminal(root_session_id),
                    new_turn_id,
                    None,
                    None,
                )
                .await
                .expect("new generation terminal"),
            AdmittedTerminalCommit::Applied
        );
    }

    #[tokio::test]
    async fn durable_mail_append_revalidates_caller_generation_in_recipient_transaction() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let recipient = retained_test_agent(
            &store,
            root_session_id,
            root_session_id,
            "/root",
            "recipient",
        )
        .await;
        let (old_admission_id, old_turn_id) = active_turn(&store, root_session_id).await;
        let recipient_history_before = store
            .protocol_event_store()
            .list_history_items_for_session(recipient.id)
            .expect("recipient history before Stop")
            .len();
        repository
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("caller Stop fence")
            .expect("caller generation boundary");

        for trigger_turn in [false, true] {
            repository
                .append_inter_agent_communication_for_caller_turn_with_protocol_bundle_and_capacity(
                    root_session_id,
                    old_admission_id,
                    old_turn_id,
                    recipient.id,
                    InterAgentCommunication {
                        author: "/root".to_string(),
                        recipient: "/root/recipient".to_string(),
                        content: render_inter_agent_message(
                            if trigger_turn {
                                InterAgentMessageType::NewTask
                            } else {
                                InterAgentMessageType::Message
                            },
                            "/root/recipient",
                            "/root",
                            "stale caller mail",
                        ),
                        trigger_turn,
                    },
                    false,
                    true,
                )
                .expect_err("pre-fence caller must not append mail after Stop");
        }
        assert_eq!(
            store
                .protocol_event_store()
                .list_history_items_for_session(recipient.id)
                .expect("recipient history after rejected mail")
                .len(),
            recipient_history_before
        );

        let user_stop_terminal = RunEvent::TurnTerminal {
            session_id: root_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    old_admission_id,
                    &user_stop_terminal,
                    old_turn_id,
                    None,
                    None,
                )
                .await
                .expect("close old caller generation"),
            AdmittedTerminalCommit::Applied
        );
        let (new_admission_id, new_turn_id) = active_turn(&store, root_session_id).await;
        let stored = repository
            .append_inter_agent_communication_for_caller_turn_with_protocol_bundle_and_capacity(
                root_session_id,
                new_admission_id,
                new_turn_id,
                recipient.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/recipient".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/recipient",
                        "/root",
                        "new generation followup",
                    ),
                    trigger_turn: true,
                },
                false,
                true,
            )
            .expect("post-fence caller mail");
        assert!(stored.schedule_turn);
        assert_eq!(
            store
                .protocol_event_store()
                .list_history_items_for_session(recipient.id)
                .expect("recipient history after valid mail")
                .len(),
            recipient_history_before
        );
        assert_eq!(
            repository
                .agent_mailbox_communications_by_id(recipient.id, &[stored.history_item_id])
                .expect("recipient pending mailbox")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn observed_running_turn_creates_then_reuses_service_stop_fence_without_extending_boundary()
     {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (admission_id, turn_id) = active_turn(&store, root_session_id).await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (_child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        for invalid_turn_id in [TurnId::new(), child_turn_id] {
            repository
                .record_agent_tree_stop_fence_for_observed_turn(
                    root_session_id,
                    crate::protocol::TurnInterruptionCause::UserStop,
                    invalid_turn_id,
                )
                .await
                .expect_err("foreign or nonexistent observed turn must fail closed");
        }
        assert_eq!(
            repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("fence count after invalid handles"),
            0
        );
        let user_stop_terminal = RunEvent::TurnTerminal {
            session_id: root_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    admission_id,
                    &user_stop_terminal,
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("worker Stop terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("fence count after exact worker Stop"),
            0,
            "an ordinary exact worker Stop must not create a subtree fence"
        );
        let first_fence = repository
            .record_agent_tree_stop_fence_for_observed_turn(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
                turn_id,
            )
            .await
            .expect("service creates observed-turn fence")
            .expect("observed turn owns a tree-Stop boundary");
        assert_eq!(
            repository
                .record_agent_tree_stop_fence_for_observed_turn(
                    root_session_id,
                    crate::protocol::TurnInterruptionCause::UserStop,
                    turn_id,
                )
                .await
                .expect("service reuses observed-turn fence"),
            Some(first_fence)
        );
        let count_after_observed_reuse = repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*) FROM agent_tree_stop_fences WHERE stopped_session_id = ?1",
                params![root_session_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("fence count after observed reuse");
        assert_eq!(count_after_observed_reuse, 1);
    }

    #[tokio::test]
    async fn legacy_completed_early_generation_is_discarded_by_a_later_fence() {
        let (store, root_session_id) = test_repo().await;
        let parent =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "parent").await;
        let owner =
            retained_test_agent(&store, root_session_id, parent.id, "/root/parent", "owner").await;
        let leaf = retained_test_agent(
            &store,
            root_session_id,
            owner.id,
            "/root/parent/owner",
            "leaf",
        )
        .await;
        let (parent_admission, parent_turn) = active_turn(&store, parent.id).await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
        let repository = store.session_repo();

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    parent.id,
                    parent_admission,
                    &agent_interrupted_terminal(parent.id),
                    parent_turn,
                    None,
                    None,
                )
                .await
                .expect("cancel reusable parent"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("pre-fence CompletedEarly"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store, owner.id, owner_turn, parent.id,
        );
        assert!(
            repository
                .pending_deferred_completion(owner.id)
                .expect("pre-fence effective deferred")
                .is_some()
        );
        let fence = repository
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("record tree Stop")
            .expect("stored tree Stop fence");
        assert!(
            repository
                .pending_deferred_completion(owner.id)
                .expect("post-fence effective deferred")
                .is_none(),
            "a committed fence must hide stale pending work before fanout settles"
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, owner_turn)
                .expect("physical fence effects")
                .deferred
                .expect("physical pre-fence deferred receipt")
                .state,
            DeferredAgentCompletionState::Discarded,
            "the fence must free the unique pending slot, not only hide it through a view"
        );

        let new_leaf = retained_test_agent(
            &store,
            root_session_id,
            owner.id,
            "/root/parent/owner",
            "new_leaf",
        )
        .await;
        let (new_leaf_admission, new_leaf_turn) = active_turn(&store, new_leaf.id).await;
        let (new_owner_admission, new_owner_turn) = active_turn(&store, owner.id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    new_owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    new_owner_turn,
                    None,
                    None,
                )
                .await
                .expect("post-fence CompletedEarly"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            owner.id,
            new_owner_turn,
            parent.id,
        );
        assert_eq!(
            repository
                .pending_deferred_completion(owner.id)
                .expect("post-fence pending deferred")
                .expect("new generation owns pending slot")
                .agent_turn_id,
            new_owner_turn
        );

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.id,
                    leaf_admission,
                    &agent_interrupted_terminal(leaf.id),
                    leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("late old worker interruption"),
            AdmittedTerminalCommit::NotOwned
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.id,
                    leaf_admission,
                    &tree_stopped_terminal(leaf.id),
                    leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("compatible post-fence descendant settlement"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .pending_deferred_completion(owner.id)
                .expect("new pending after old terminal")
                .expect("late old terminal must not discard new generation")
                .agent_turn_id,
            new_owner_turn
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, owner_turn)
                .expect("fenced owner effects")
                .deferred
                .expect("physical deferred receipt")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            repository
                .agent_completion_handoff(owner.id, owner_turn)
                .expect("fenced owner handoff")
                .is_none()
        );
        assert!(
            repository
                .pending_agent_trigger_history_item_id_for_tree_stop(leaf.id, fence)
                .expect("exact Stop lookup")
                .is_none()
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    new_leaf.id,
                    new_leaf_admission,
                    &completed_terminal_for_response(new_leaf.id, None),
                    new_leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("post-fence child result"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, new_owner_turn)
                .expect("new generation owner effects")
                .deferred
                .expect("new generation deferred receipt")
                .state,
            DeferredAgentCompletionState::Superseded
        );
    }

    #[tokio::test]
    async fn synthetic_stop_settlement_uses_first_fence_and_rejects_out_of_boundary_trigger() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let repository = store.session_repo();
        let old = repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/child",
                        "/root",
                        "old trigger",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("old trigger");
        let first_fence = repository
            .record_agent_tree_stop_fence(
                child.id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("first child Stop")
            .expect("first child fence");
        let new = repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/child",
                        "/root",
                        "post-first-fence trigger",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("new trigger");
        let _later_fence = repository
            .record_agent_tree_stop_fence(
                child.id,
                crate::protocol::TurnInterruptionCause::ApprovalAborted,
            )
            .await
            .expect("later child Stop")
            .expect("later child fence");

        assert!(matches!(
            repository
                .settle_pending_agent_trigger_at_tree_stop_fence(
                    child.id,
                    new.history_item_id,
                    first_fence,
                )
                .expect("out-of-boundary settlement"),
            PendingAgentTriggerSettlement::WakeOwnedOrResolved
        ));
        assert!(matches!(
            repository
                .settle_pending_agent_trigger_with_terminal(
                    child.id,
                    old.history_item_id,
                    DurableTurnTerminal {
                        outcome: TurnTerminalOutcome::Interrupted {
                            cause: crate::protocol::TurnInterruptionCause::UserStop,
                        },
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    },
                )
                .expect("generic fenced settlement"),
            PendingAgentTriggerSettlement::WakeOwnedOrResolved
        ));
        let PendingAgentTriggerSettlement::Applied {
            turn_id: settlement_turn_id,
            ..
        } = repository
            .settle_pending_agent_trigger_at_tree_stop_fence(
                child.id,
                old.history_item_id,
                first_fence,
            )
            .expect("exact fenced settlement")
        else {
            panic!("exact fenced settlement must claim the old trigger");
        };
        assert!(matches!(
            repository
                .durable_terminal_for_turn(child.id, settlement_turn_id)
                .await
                .expect("synthetic terminal")
                .expect("synthetic durable terminal")
                .outcome,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop
            }
        ));
        let fence_count = repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*)
                 FROM agent_tree_stop_fences
                 WHERE stopped_session_id = ?1",
                params![child.id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("child fence count");
        assert_eq!(
            fence_count, 2,
            "synthetic settlement must reuse the first fence rather than creating a third"
        );
    }

    #[tokio::test]
    async fn fence_hides_old_trigger_from_restart_context_but_allows_new_explicit_followup() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let repository = store.session_repo();
        let old = repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/child",
                        "/root",
                        "old task must not replay",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("old pending trigger");
        let fence = repository
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("record context fence")
            .expect("stored context fence");
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(child.id)
                .expect("normal old trigger lookup"),
            None
        );
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id_for_tree_stop(child.id, fence)
                .expect("Stop old trigger lookup"),
            Some(old.history_item_id)
        );

        let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen fenced context");
        reopened_sqlite.migrate().expect("migrate reopened context");
        let reopened = StoreBundle::new(reopened_sqlite);
        let mut active_before_followup = Vec::new();
        reopened
            .protocol_event_store()
            .visit_active_history_pages_for_session(
                child.id,
                crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                &mut |page| {
                    active_before_followup.extend(page.items);
                    Ok(())
                },
            )
            .expect("fenced active context");
        assert!(
            active_before_followup
                .iter()
                .all(|item| item.id != old.history_item_id)
        );

        let followup = reopened
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/child",
                        "/root",
                        "new explicit followup",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("post-fence explicit followup");
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id_for_tree_stop(child.id, fence)
                .expect("exact old boundary after followup"),
            Some(old.history_item_id),
            "a later followup must not replace the trigger selected for an earlier Stop"
        );
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("new trigger lookup"),
            Some(followup.history_item_id)
        );
        let followup_turn = TurnId::new();
        let admitted = reopened
            .session_repo()
            .admit_agent_triggered_turn(child.id, followup_turn, followup.history_item_id)
            .await
            .expect("followup admission")
            .expect("post-fence followup admitted");
        assert_eq!(
            reopened
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    child.id,
                    admitted.admission_id,
                    followup_turn,
                    128,
                )
                .expect("safe post-fence followup delivery")
                .history_item_ids,
            vec![followup.history_item_id]
        );
        let mut followup_context_items = Vec::new();
        reopened
            .protocol_event_store()
            .visit_active_history_pages_for_session(
                child.id,
                crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                &mut |page| {
                    followup_context_items.extend(page.items);
                    Ok(())
                },
            )
            .expect("post-fence model context");
        let model_messages =
            crate::agent::context_manager::ContextManager::rehydrate(followup_context_items)
                .model_messages(false);
        assert!(model_messages.iter().any(|message| matches!(
            message,
            crate::llm::ModelMessage::Agent { content }
                if content.contains("new explicit followup")
        )));
        assert!(model_messages.iter().all(|message| !matches!(
            message,
            crate::llm::ModelMessage::Agent { content }
                if content.contains("old task must not replay")
        )));
        assert_eq!(
            reopened
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    admitted.admission_id,
                    &completed_terminal_for_response(child.id, None),
                    followup_turn,
                    None,
                    None,
                )
                .await
                .expect("post-fence followup terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            reopened
                .session_repo()
                .agent_completion_handoff(child.id, followup_turn)
                .expect("post-fence followup handoff")
                .is_some()
        );
    }

    #[tokio::test]
    async fn legacy_completed_early_releases_once_after_last_agent_interruption() {
        let (store, root_session_id) = test_repo().await;
        let owner =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
        let child =
            retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (child_admission, child_turn) = active_turn(&store, child.id).await;

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("owner early completion"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            owner.id,
            owner_turn,
            root_session_id,
        );
        assert!(
            store
                .session_repo()
                .agent_completion_handoff(owner.id, owner_turn)
                .expect("early owner handoff")
                .is_none()
        );
        assert_eq!(
            store
                .session_repo()
                .pending_deferred_completion(owner.id)
                .expect("pending owner deferred")
                .expect("owner deferred")
                .kind,
            DeferredAgentCompletionKind::CompletedEarly
        );

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission,
                    &agent_interrupted_terminal(child.id),
                    child_turn,
                    None,
                    None,
                )
                .await
                .expect("child interruption"),
            AdmittedTerminalCommit::Applied
        );
        let effects = store
            .session_repo()
            .agent_terminal_effects(child.id, child_turn)
            .expect("interruption effects");
        assert_eq!(effects.released_deferred_handoffs.len(), 1);
        assert_eq!(
            effects.released_deferred_handoffs[0].child_session_id,
            owner.id
        );
        assert_eq!(
            store
                .session_repo()
                .agent_terminal_effects(owner.id, owner_turn)
                .expect("owner deferred effects")
                .deferred
                .expect("owner deferred receipt")
                .state,
            DeferredAgentCompletionState::Released
        );
        assert!(
            store
                .session_repo()
                .pending_deferred_completion(owner.id)
                .expect("resolved owner deferred")
                .is_none()
        );
    }

    #[tokio::test]
    async fn legacy_completed_early_snapshot_keeps_release_and_trigger_readiness_atomic() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let owner =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
        let child =
            retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (child_admission, child_turn) = active_turn(&store, child.id).await;
        let owner_response = record_text_response(
            &store,
            owner.id,
            owner_admission,
            owner_turn,
            "owner result",
        )
        .await;

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, Some(owner_response)),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("owner completed early"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            owner.id,
            owner_turn,
            root_session_id,
        );
        let explicit = repository
            .append_inter_agent_communication_with_protocol_bundle(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner",
                        "/root",
                        "queued while owner awaits child",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("deferred explicit trigger");
        assert!(!explicit.schedule_turn);
        let snapshot_limit = crate::runtime::agent_control::MAX_RETAINED_AGENTS.saturating_sub(1);
        let before = store
            .protocol_event_store()
            .retained_descendant_snapshot(root_session_id, snapshot_limit)
            .expect("pre-release retained snapshot");
        let before_owner = before
            .iter()
            .find(|item| item.edge.child_session_id == owner.id)
            .expect("pre-release owner");
        assert_eq!(before_owner.pending_deferred_turn_id, Some(owner_turn));
        assert_eq!(
            before_owner.pending_deferred_completion_kind,
            Some(DeferredAgentCompletionKind::CompletedEarly)
        );
        assert_eq!(
            before_owner.pending_trigger_history_item_id,
            Some(explicit.history_item_id)
        );
        assert!(!before_owner.pending_trigger_schedule_ready);
        assert_eq!(before_owner.pending_owner_resume_request_id, None);
        assert_eq!(before_owner.session_status, "completed");
        assert!(matches!(
            before_owner.latest_task_content.as_deref(),
            Some([ContentPart::Text { text }]) if text == "canonical request"
        ));
        assert!(matches!(
            before_owner.latest_assistant_content.as_deref(),
            Some([ContentPart::Text { text }]) if text == "owner result"
        ));
        assert_eq!(before_owner.latest_error, None);
        assert_eq!(before_owner.interruption_cause, None);

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission,
                    &completed_terminal_for_response(child.id, None),
                    child_turn,
                    None,
                    None,
                )
                .await
                .expect("child terminal releases owner"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = repository
            .agent_completion_handoff(child.id, child_turn)
            .expect("child handoff")
            .expect("stored child handoff");
        assert_eq!(handoff.released_owner_deferred_turn_id, Some(owner_turn));
        let reopened_sqlite =
            SqliteStore::open(store.paths()).expect("reopen released retained tree");
        reopened_sqlite
            .migrate()
            .expect("migrate reopened retained tree");
        let reopened = StoreBundle::new(reopened_sqlite);
        let after = reopened
            .protocol_event_store()
            .retained_descendant_snapshot(root_session_id, snapshot_limit)
            .expect("post-release retained snapshot");
        let after_owner = after
            .iter()
            .find(|item| item.edge.child_session_id == owner.id)
            .expect("post-release owner");
        assert_eq!(after_owner.pending_deferred_turn_id, None);
        assert_eq!(after_owner.pending_deferred_completion_kind, None);
        assert_eq!(
            after_owner.pending_trigger_history_item_id,
            Some(explicit.history_item_id)
        );
        assert!(after_owner.pending_trigger_schedule_ready);
        let raw_owner_resume = repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("raw owner resume after child completion");
        assert_eq!(raw_owner_resume.len(), 1);
        assert_eq!(
            after_owner.pending_owner_resume_request_id,
            Some(raw_owner_resume[0].request_id),
            "the snapshot intentionally carries raw scheduler ownership; explicit admission coalesces it"
        );
        assert_eq!(after_owner.session_status, "completed");
        assert!(matches!(
            after_owner.latest_task_content.as_deref(),
            Some([ContentPart::Text { text }]) if text == "canonical request"
        ));
        assert!(matches!(
            after_owner.latest_assistant_content.as_deref(),
            Some([ContentPart::Text { text }]) if text == "owner result"
        ));
        assert_eq!(after_owner.latest_error, None);
        assert_eq!(after_owner.interruption_cause, None);
    }

    #[tokio::test]
    async fn retained_snapshot_pages_over_two_hundred_rows_in_one_read_transaction() {
        let (store, root_session_id) = test_repo().await;
        let descendant_count = crate::protocol::MAX_PROTOCOL_PAGE_LIMIT + 1;
        let mut descendants = Vec::with_capacity(descendant_count);
        for index in 0..descendant_count {
            descendants.push(
                retained_test_agent(
                    &store,
                    root_session_id,
                    root_session_id,
                    "/root",
                    &format!("paged_{index:03}"),
                )
                .await,
            );
        }
        let second_page_child = descendants[crate::protocol::MAX_PROTOCOL_PAGE_LIMIT].id;
        let reopened_sqlite =
            SqliteStore::open(store.paths()).expect("open independent snapshot writer");
        reopened_sqlite
            .migrate()
            .expect("migrate independent snapshot writer");
        let competing_store = StoreBundle::new(reopened_sqlite);
        let mut observed_page_ends = Vec::new();
        let mut mutated_between_pages = false;
        let mut page_boundary_trigger_id = None;

        let snapshot = store
            .protocol_event_store()
            .retained_descendant_snapshot_observing_pages(
                root_session_id,
                descendant_count,
                &mut |read_count| {
                    observed_page_ends.push(read_count);
                    if read_count == crate::protocol::MAX_PROTOCOL_PAGE_LIMIT {
                        let appended = competing_store
                            .session_repo()
                            .append_inter_agent_communication_with_protocol_bundle(
                                second_page_child,
                                InterAgentCommunication {
                                    author: "/root".to_string(),
                                    recipient: format!(
                                        "/root/paged_{:03}",
                                        crate::protocol::MAX_PROTOCOL_PAGE_LIMIT
                                    ),
                                    content: render_inter_agent_message(
                                        InterAgentMessageType::NewTask,
                                        &format!(
                                            "/root/paged_{:03}",
                                            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT
                                        ),
                                        "/root",
                                        "valid page-boundary followup",
                                    ),
                                    trigger_turn: true,
                                },
                                false,
                            )?;
                        if !appended.schedule_turn {
                            return Err(StorageError::Message(
                                "page-boundary followup was not a schedulable first trigger"
                                    .to_string(),
                            ));
                        }
                        page_boundary_trigger_id = Some(appended.history_item_id);
                        mutated_between_pages = true;
                    }
                    Ok(())
                },
            )
            .expect("multi-page retained snapshot");

        assert!(mutated_between_pages);
        assert_eq!(
            observed_page_ends,
            vec![crate::protocol::MAX_PROTOCOL_PAGE_LIMIT, descendant_count]
        );
        assert_eq!(snapshot.len(), descendant_count);
        assert_eq!(
            snapshot
                .iter()
                .find(|item| item.edge.child_session_id == second_page_child)
                .expect("second-page child in stable snapshot")
                .pending_trigger_history_item_id,
            None,
            "the second page must retain the trigger state established by the first-page read snapshot"
        );
        let exact_boundary_trigger =
            page_boundary_trigger_id.expect("valid page-boundary followup identity");
        assert_eq!(
            store
                .protocol_event_store()
                .retained_descendant_page(
                    root_session_id,
                    crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                    1,
                )
                .expect("fresh page after snapshot")
                .items[0]
                .pending_trigger_history_item_id,
            Some(exact_boundary_trigger),
            "a later transaction must observe the committed page-boundary mutation"
        );
        assert!(
            store
                .protocol_event_store()
                .retained_descendant_page(
                    root_session_id,
                    crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                    1,
                )
                .expect("fresh readiness page after snapshot")
                .items[0]
                .pending_trigger_schedule_ready
        );

        let zero_error = store
            .protocol_event_store()
            .retained_descendant_snapshot(root_session_id, 0)
            .expect_err("zero snapshot bound must be rejected");
        assert!(zero_error.to_string().contains("must be at least 1"));
        let bounded_error = store
            .protocol_event_store()
            .retained_descendant_snapshot(root_session_id, crate::protocol::MAX_PROTOCOL_PAGE_LIMIT)
            .expect_err("a custom bound below the retained count must reject truncation");
        assert!(
            bounded_error
                .to_string()
                .contains("exceeding the supported maximum 200")
        );
    }

    #[tokio::test]
    async fn tree_stop_discards_legacy_completed_early_without_releasing_final() {
        let (store, root_session_id) = test_repo().await;
        let owner =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
        let child =
            retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (child_admission, child_turn) = active_turn(&store, child.id).await;
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("owner early completion"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            owner.id,
            owner_turn,
            root_session_id,
        );
        assert!(
            store
                .session_repo()
                .record_agent_tree_stop_fence(
                    root_session_id,
                    crate::protocol::TurnInterruptionCause::UserStop,
                )
                .await
                .expect("record explicit root tree-Stop boundary")
                .is_some()
        );
        let stopped = RunEvent::TurnTerminal {
            session_id: child.id,
            terminal: Box::new(pre_admission_interrupted_terminal()),
        };
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission,
                    &stopped,
                    child_turn,
                    None,
                    None,
                )
                .await
                .expect("tree-stopped child"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .agent_terminal_effects(owner.id, owner_turn)
                .expect("discarded owner deferred")
                .deferred
                .expect("owner deferred receipt")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            store
                .session_repo()
                .agent_completion_handoff(owner.id, owner_turn)
                .expect("discarded owner handoff")
                .is_none()
        );
        assert!(
            store
                .session_repo()
                .agent_terminal_effects(child.id, child_turn)
                .expect("tree-stop effects")
                .released_deferred_handoffs
                .is_empty()
        );
        let project_id = store
            .session_repo()
            .get_session(root_session_id)
            .await
            .expect("root before subtree delete")
            .project_id;
        store
            .session_repo()
            .delete_session_tree(root_session_id)
            .await
            .expect("delete subtree with discarded deferred receipt");
        store
            .project_repo()
            .delete_project(project_id)
            .await
            .expect("delete project after deferred subtree cleanup");
    }

    #[tokio::test]
    async fn nested_legacy_completed_early_release_supersedes_outer_candidate() {
        let (store, root_session_id) = test_repo().await;
        let outer =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "outer").await;
        let middle =
            retained_test_agent(&store, root_session_id, outer.id, "/root/outer", "middle").await;
        let leaf = retained_test_agent(
            &store,
            root_session_id,
            middle.id,
            "/root/outer/middle",
            "leaf",
        )
        .await;
        let (outer_admission, outer_turn) = active_turn(&store, outer.id).await;
        let (middle_admission, middle_turn) = active_turn(&store, middle.id).await;
        let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;

        for (session_id, admission_id, turn_id, parent_session_id) in [
            (middle.id, middle_admission, middle_turn, outer.id),
            (outer.id, outer_admission, outer_turn, root_session_id),
        ] {
            assert_eq!(
                store
                    .session_repo()
                    .terminalize_admitted_turn_with_protocol_event(
                        session_id,
                        admission_id,
                        &completed_terminal_for_response(session_id, None),
                        turn_id,
                        None,
                        None,
                    )
                    .await
                    .expect("nested early completion"),
                AdmittedTerminalCommit::Applied
            );
            replace_current_handoff_with_legacy_completed_early(
                &store,
                session_id,
                turn_id,
                parent_session_id,
            );
        }

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.id,
                    leaf_admission,
                    &agent_interrupted_terminal(leaf.id),
                    leaf_turn,
                    None,
                    None,
                )
                .await
                .expect("leaf interruption"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .agent_terminal_effects(middle.id, middle_turn)
                .expect("middle deferred")
                .deferred
                .expect("middle deferred receipt")
                .state,
            DeferredAgentCompletionState::Released
        );
        assert_eq!(
            store
                .session_repo()
                .agent_terminal_effects(outer.id, outer_turn)
                .expect("outer deferred")
                .deferred
                .expect("outer deferred receipt")
                .state,
            DeferredAgentCompletionState::Superseded
        );
        assert!(
            store
                .session_repo()
                .agent_completion_handoff(outer.id, outer_turn)
                .expect("outer handoff")
                .is_none(),
            "the outer owner must resume instead of leaking its early FINAL"
        );
        assert!(
            store
                .session_repo()
                .schedulable_owner_resume_request_id(outer.id)
                .expect("outer OwnerResume")
                .is_some()
        );
    }

    #[tokio::test]
    async fn expired_claimed_owner_resume_recovers_without_leaking_crash_final() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let crashed_turn = TurnId::new();
        let crashed_admission = repository
            .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
            .await
            .expect("initial OwnerResume admission")
            .expect("initial OwnerResume admitted");
        assert_eq!(
            repository
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    crashed_admission.admission_id,
                    crashed_turn,
                    128,
                )
                .expect("safe claimed OwnerResume delivery")
                .history_item_ids,
            vec![requests[0].request_id.0]
        );
        repository
            .inject_raw_runtime_state_for_corruption_test(
                owner.id,
                "running",
                Some(&crashed_admission.admission_id.to_string()),
                Some(&crashed_turn.to_string()),
                Some(1),
            )
            .expect("expire claimed OwnerResume");

        let recovered_request = repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("recovered request lookup")
            .unwrap_or(requests[0].request_id);
        let continuation_turn = TurnId::new();
        let continuation_admission = repository
            .admit_owner_resume_turn(owner.id, continuation_turn, recovered_request)
            .await
            .expect("replacement OwnerResume admission")
            .expect("replacement OwnerResume admitted");
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("crash deferred effects")
                .deferred
                .expect("crash deferred receipt")
                .state,
            DeferredAgentCompletionState::Pending
        );
        assert!(
            repository
                .agent_completion_handoff(owner.id, crashed_turn)
                .expect("crash handoff lookup")
                .is_none(),
            "recoverable crash failure must not escape upward"
        );

        repository
            .inject_raw_runtime_state_for_corruption_test(
                owner.id,
                "running",
                Some(&continuation_admission.admission_id.to_string()),
                Some(&continuation_turn.to_string()),
                Some(1),
            )
            .expect("expire replacement OwnerResume");
        let final_turn = TurnId::new();
        let final_admission = repository
            .admit_owner_resume_turn(owner.id, final_turn, recovered_request)
            .await
            .expect("second replacement OwnerResume admission")
            .expect("second replacement OwnerResume admitted");
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("first crash resolution")
                .deferred
                .expect("first crash deferred")
                .state,
            DeferredAgentCompletionState::Superseded
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, continuation_turn)
                .expect("second crash pending")
                .deferred
                .expect("second crash deferred")
                .state,
            DeferredAgentCompletionState::Pending
        );

        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    final_admission.admission_id,
                    &completed_terminal_for_response(owner.id, None),
                    final_turn,
                    None,
                    None,
                )
                .await
                .expect("replacement OwnerResume terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            repository
                .agent_completion_handoff(owner.id, final_turn)
                .expect("replacement handoff")
                .is_some()
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, continuation_turn)
                .expect("resolved crash deferred effects")
                .deferred
                .expect("resolved crash deferred receipt")
                .state,
            DeferredAgentCompletionState::Superseded
        );
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM agent_completion_handoffs
                     WHERE child_session_id = ?1",
                    params![owner.id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("owner upstream handoff count"),
            1
        );
    }

    #[tokio::test]
    async fn admitted_replacement_owner_resume_interruption_discards_crash_deferred() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let crashed_turn = TurnId::new();
        let crashed_admission = repository
            .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
            .await
            .expect("initial OwnerResume admission")
            .expect("initial OwnerResume admitted");
        assert_eq!(
            repository
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    crashed_admission.admission_id,
                    crashed_turn,
                    128,
                )
                .expect("safe child-result delivery")
                .history_item_ids,
            vec![requests[0].request_id.0]
        );
        let target = repository
            .captured_running_terminal_target(owner.id)
            .await
            .expect("capture OwnerResume")
            .expect("running OwnerResume");
        assert!(
            repository
                .recover_captured_running_session_with_protocol_event(
                    owner.id,
                    &failed_terminal(owner.id, "owner process crashed"),
                    target,
                )
                .await
                .expect("recover crashed OwnerResume")
        );

        let replacement_turn = TurnId::new();
        let replacement = repository
            .admit_owner_resume_turn(owner.id, replacement_turn, requests[0].request_id)
            .await
            .expect("replacement OwnerResume admission")
            .expect("replacement OwnerResume admitted");
        let interrupted = RunEvent::TurnTerminal {
            session_id: owner.id,
            terminal: Box::new(pre_admission_agent_interrupted_terminal()),
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    replacement.admission_id,
                    &interrupted,
                    replacement_turn,
                    None,
                    None,
                )
                .await
                .expect("interrupt replacement OwnerResume"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("discarded crash effects")
                .deferred
                .expect("crash deferred receipt")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            repository
                .agent_completion_handoff(owner.id, replacement_turn)
                .expect("interrupted replacement handoff")
                .is_none(),
            "an interrupted OwnerResume must not escape upward"
        );
        assert!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("resolved OwnerResume")
                .is_none()
        );

        let project_id = repository
            .get_session(root_session_id)
            .await
            .expect("root before cleanup")
            .project_id;
        repository
            .delete_session_tree(root_session_id)
            .await
            .expect("discarded crash deferred must not block tree deletion");
        store
            .project_repo()
            .delete_project(project_id)
            .await
            .expect("discarded crash deferred must not block project deletion");
    }

    #[tokio::test]
    async fn explicit_trigger_recovers_pending_crash_owner_resume() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let crashed_turn = TurnId::new();
        let crashed_admission = repository
            .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
            .await
            .expect("initial OwnerResume admission")
            .expect("initial OwnerResume admitted");
        assert_eq!(
            repository
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    crashed_admission.admission_id,
                    crashed_turn,
                    128,
                )
                .expect("safe child-result delivery")
                .history_item_ids,
            vec![requests[0].request_id.0]
        );
        let target = repository
            .captured_running_terminal_target(owner.id)
            .await
            .expect("capture OwnerResume")
            .expect("running OwnerResume");
        assert!(
            repository
                .recover_captured_running_session_with_protocol_event(
                    owner.id,
                    &failed_terminal(owner.id, "owner process crashed"),
                    target,
                )
                .await
                .expect("recover crashed OwnerResume")
        );
        let explicit = repository
            .append_inter_agent_communication_with_protocol_bundle(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner",
                        "/root",
                        "explicit recovery work",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("explicit recovery trigger");
        assert!(explicit.schedule_turn);
        let projection = store
            .protocol_event_store()
            .retained_descendant_page(root_session_id, 0, 10)
            .expect("explicit crash recovery projection");
        let projected_owner = projection
            .items
            .iter()
            .find(|item| item.edge.child_session_id == owner.id)
            .expect("projected crash owner");
        assert_eq!(
            projected_owner.pending_trigger_history_item_id,
            Some(explicit.history_item_id)
        );
        assert!(projected_owner.pending_trigger_schedule_ready);
        assert_eq!(
            projected_owner.pending_owner_resume_request_id,
            Some(requests[0].request_id)
        );
        let recovery_turn = TurnId::new();
        let recovery = repository
            .admit_agent_triggered_turn(owner.id, recovery_turn, explicit.history_item_id)
            .await
            .expect("explicit recovery admission")
            .expect("explicit recovery admitted");
        assert_eq!(
            repository
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    recovery.admission_id,
                    recovery_turn,
                    128,
                )
                .expect("safe explicit-recovery delivery")
                .history_item_ids,
            vec![explicit.history_item_id]
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    recovery.admission_id,
                    &completed_terminal_for_response(owner.id, None),
                    recovery_turn,
                    None,
                    None,
                )
                .await
                .expect("explicit recovery terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("superseded crash effects")
                .deferred
                .expect("crash deferred receipt")
                .state,
            DeferredAgentCompletionState::Superseded
        );
        assert!(
            repository
                .agent_completion_handoff(owner.id, recovery_turn)
                .expect("explicit recovery handoff")
                .is_some()
        );
        assert!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("resolved coalesced OwnerResume")
                .is_none()
        );
    }

    #[tokio::test]
    async fn synthetic_explicit_stop_discards_pending_crash_owner_resume() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let crashed_turn = TurnId::new();
        let crashed_admission = repository
            .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
            .await
            .expect("initial OwnerResume admission")
            .expect("initial OwnerResume admitted");
        assert_eq!(
            repository
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    crashed_admission.admission_id,
                    crashed_turn,
                    128,
                )
                .expect("safe child-result delivery")
                .history_item_ids,
            vec![requests[0].request_id.0]
        );
        let target = repository
            .captured_running_terminal_target(owner.id)
            .await
            .expect("capture OwnerResume")
            .expect("running OwnerResume");
        assert!(
            repository
                .recover_captured_running_session_with_protocol_event(
                    owner.id,
                    &failed_terminal(owner.id, "owner process crashed"),
                    target,
                )
                .await
                .expect("recover crashed OwnerResume")
        );
        let explicit = repository
            .append_inter_agent_communication_with_protocol_bundle(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner",
                        "/root",
                        "explicit recovery work",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("explicit recovery trigger");
        assert!(explicit.schedule_turn);
        assert!(matches!(
            repository
                .settle_pending_agent_trigger_with_terminal(
                    owner.id,
                    explicit.history_item_id,
                    pre_admission_interrupted_terminal(),
                )
                .expect("synthetic stop explicit recovery"),
            PendingAgentTriggerSettlement::Applied { handoff: None, .. }
        ));
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("discarded crash effects")
                .deferred
                .expect("crash deferred receipt")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("resolved coalesced OwnerResume")
                .is_none()
        );
    }

    #[tokio::test]
    async fn legacy_completed_early_blocks_synthetic_terminal_until_exact_stop() {
        for terminal in [
            pre_admission_failed_terminal("launch failed while owner awaits descendants"),
            pre_admission_agent_interrupted_terminal(),
        ] {
            let (store, root_session_id) = test_repo().await;
            let repository = store.session_repo();
            let owner = retained_test_agent(
                &store,
                root_session_id,
                root_session_id,
                "/root",
                "completed_owner",
            )
            .await;
            let child = retained_test_agent(
                &store,
                root_session_id,
                owner.id,
                "/root/completed_owner",
                "child",
            )
            .await;
            let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
            let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
            assert_eq!(
                repository
                    .terminalize_admitted_turn_with_protocol_event(
                        owner.id,
                        owner_admission,
                        &completed_terminal_for_response(owner.id, None),
                        owner_turn,
                        None,
                        None,
                    )
                    .await
                    .expect("early owner completion"),
                AdmittedTerminalCommit::Applied
            );
            replace_current_handoff_with_legacy_completed_early(
                &store,
                owner.id,
                owner_turn,
                root_session_id,
            );
            let explicit = repository
                .append_inter_agent_communication_with_protocol_bundle_and_capacity(
                    owner.id,
                    InterAgentCommunication {
                        author: "/root".to_string(),
                        recipient: "/root/completed_owner".to_string(),
                        content: render_inter_agent_message(
                            InterAgentMessageType::NewTask,
                            "/root/completed_owner",
                            "/root",
                            "preserve this exact trigger",
                        ),
                        trigger_turn: true,
                    },
                    false,
                    false,
                )
                .expect("completed-owner explicit trigger");
            assert!(!explicit.schedule_turn);

            for _ in 0..2 {
                assert_eq!(
                    repository
                        .settle_pending_agent_trigger_with_terminal(
                            owner.id,
                            explicit.history_item_id,
                            terminal.clone(),
                        )
                        .expect("blocked non-destructive settlement"),
                    PendingAgentTriggerSettlement::BlockedByPendingDeferredCompletion {
                        deferred_turn_id: owner_turn
                    }
                );
                assert_eq!(
                    repository
                        .pending_agent_trigger_history_item_id(owner.id)
                        .expect("retained exact trigger"),
                    Some(explicit.history_item_id)
                );
                let deferred = repository
                    .pending_deferred_completion(owner.id)
                    .expect("pending deferred query")
                    .expect("completed-early deferred remains pending");
                assert_eq!(deferred.agent_turn_id, owner_turn);
                assert_eq!(deferred.state, DeferredAgentCompletionState::Pending);
            }

            assert!(matches!(
                repository
                    .settle_pending_agent_trigger_with_terminal(
                        owner.id,
                        explicit.history_item_id,
                        pre_admission_user_stopped_terminal(),
                    )
                    .expect("destructive stop settlement"),
                PendingAgentTriggerSettlement::Applied { handoff: None, .. }
            ));
            assert!(
                repository
                    .pending_agent_trigger_history_item_id(owner.id)
                    .expect("settled trigger")
                    .is_none()
            );
            assert_eq!(
                repository
                    .agent_terminal_effects(owner.id, owner_turn)
                    .expect("discarded deferred effects")
                    .deferred
                    .expect("completed-early receipt")
                    .state,
                DeferredAgentCompletionState::Discarded
            );
        }
    }

    #[tokio::test]
    async fn legacy_completed_early_and_current_crash_deferred_remain_compatible() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let owner = retained_test_agent(
            &store,
            root_session_id,
            root_session_id,
            "/root",
            "completed_owner",
        )
        .await;
        let child = retained_test_agent(
            &store,
            root_session_id,
            owner.id,
            "/root/completed_owner",
            "child",
        )
        .await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("early owner completion"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            owner.id,
            owner_turn,
            root_session_id,
        );
        let completed_explicit = repository
            .append_inter_agent_communication_with_protocol_bundle_and_capacity(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/completed_owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/completed_owner",
                        "/root",
                        "must remain deferred",
                    ),
                    trigger_turn: true,
                },
                false,
                false,
            )
            .expect("completed-owner explicit trigger");
        assert!(
            !completed_explicit.schedule_turn,
            "completed-early owner mail must remain queued until descendant settlement"
        );
        let reopened_sqlite =
            SqliteStore::open(store.paths()).expect("reopen completed-early queue");
        reopened_sqlite.migrate().expect("migrate reopened store");
        let reopened = StoreBundle::new(reopened_sqlite);
        let projection = reopened
            .protocol_event_store()
            .retained_descendant_page(root_session_id, 0, 10)
            .expect("reopened completed-early projection");
        let projected_owner = projection
            .items
            .iter()
            .find(|item| item.edge.child_session_id == owner.id)
            .expect("projected completed-early owner");
        assert_eq!(
            projected_owner.pending_trigger_history_item_id,
            Some(completed_explicit.history_item_id)
        );
        assert!(!projected_owner.pending_trigger_schedule_ready);
        assert_eq!(projected_owner.pending_owner_resume_request_id, None);
        assert!(
            repository
                .admit_agent_triggered_turn(
                    owner.id,
                    TurnId::new(),
                    completed_explicit.history_item_id,
                )
                .await
                .expect("completed-owner explicit admission")
                .is_none()
        );
        assert!(matches!(
            repository
                .settle_pending_agent_trigger_with_terminal(
                    owner.id,
                    completed_explicit.history_item_id,
                    pre_admission_user_stopped_terminal(),
                )
                .expect("completed-owner synthetic settlement"),
            PendingAgentTriggerSettlement::Applied { handoff: None, .. }
        ));
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, owner_turn)
                .expect("stopped completed-early effects")
                .deferred
                .expect("completed-early deferred")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            repository
                .pending_agent_trigger_history_item_id(owner.id)
                .expect("settled completed-owner trigger")
                .is_none()
        );

        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let owner = retained_test_agent(
            &store,
            root_session_id,
            root_session_id,
            "/root",
            "crashed_owner",
        )
        .await;
        let child = retained_test_agent(
            &store,
            root_session_id,
            owner.id,
            "/root/crashed_owner",
            "child",
        )
        .await;
        let (_owner_admission, crashed_turn) = active_turn(&store, owner.id).await;
        let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
        let target = repository
            .captured_running_terminal_target(owner.id)
            .await
            .expect("capture crashed owner")
            .expect("running crashed owner");
        assert!(
            repository
                .recover_captured_running_session_with_protocol_event(
                    owner.id,
                    &failed_terminal(owner.id, "owner process crashed"),
                    target,
                )
                .await
                .expect("recover owner without OwnerResume")
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(owner.id)
                .expect("owner resume requests")
                .is_empty()
        );
        let capacity_error = repository
            .append_inter_agent_communication_with_protocol_bundle_and_capacity(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/crashed_owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/crashed_owner",
                        "/root",
                        "capacity-rejected crash recovery",
                    ),
                    trigger_turn: true,
                },
                false,
                false,
            )
            .expect_err("ready orphan crash must require an execution reservation");
        assert!(matches!(
            capacity_error,
            StorageError::AgentCapacityUnavailable { session_id }
                if session_id == owner.id
        ));
        assert!(
            repository
                .pending_agent_trigger_history_item_id(owner.id)
                .expect("capacity-rejected crash trigger")
                .is_none()
        );
        let crash_explicit = repository
            .append_inter_agent_communication_with_protocol_bundle(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/crashed_owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/crashed_owner",
                        "/root",
                        "recover orphan crash",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("orphan crash explicit trigger");
        assert!(crash_explicit.schedule_turn);
        let projection = store
            .protocol_event_store()
            .retained_descendant_page(root_session_id, 0, 10)
            .expect("orphan crash projection");
        let projected_owner = projection
            .items
            .iter()
            .find(|item| item.edge.child_session_id == owner.id)
            .expect("projected crashed owner");
        assert_eq!(
            projected_owner.pending_trigger_history_item_id,
            Some(crash_explicit.history_item_id)
        );
        assert!(projected_owner.pending_trigger_schedule_ready);
        assert_eq!(projected_owner.pending_owner_resume_request_id, None);

        let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen orphan crash store");
        reopened_sqlite.migrate().expect("migrate reopened store");
        let reopened = StoreBundle::new(reopened_sqlite);
        let reopened_projection = reopened
            .protocol_event_store()
            .retained_descendant_page(root_session_id, 0, 10)
            .expect("reopened orphan crash projection");
        let reopened_owner = reopened_projection
            .items
            .iter()
            .find(|item| item.edge.child_session_id == owner.id)
            .expect("reopened crashed owner");
        assert_eq!(
            reopened_owner.pending_trigger_history_item_id,
            Some(crash_explicit.history_item_id)
        );
        assert!(reopened_owner.pending_trigger_schedule_ready);
        assert_eq!(reopened_owner.pending_owner_resume_request_id, None);

        let recovery_turn = TurnId::new();
        let recovery = reopened
            .session_repo()
            .admit_agent_triggered_turn(owner.id, recovery_turn, crash_explicit.history_item_id)
            .await
            .expect("reopened explicit crash recovery")
            .expect("explicit crash recovery admitted");
        assert_eq!(
            reopened
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    recovery.admission_id,
                    recovery_turn,
                    128,
                )
                .expect("safe orphan-crash trigger delivery")
                .history_item_ids,
            vec![crash_explicit.history_item_id]
        );
        assert_eq!(
            reopened
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    recovery.admission_id,
                    &completed_terminal_for_response(owner.id, None),
                    recovery_turn,
                    None,
                    None,
                )
                .await
                .expect("explicit orphan-crash recovery terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            reopened
                .session_repo()
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("superseded orphan crash")
                .deferred
                .expect("orphan crash deferred")
                .state,
            DeferredAgentCompletionState::Superseded
        );
        assert!(
            reopened
                .session_repo()
                .agent_terminal_effects(owner.id, recovery_turn)
                .expect("replacement completion effects")
                .deferred
                .is_none(),
            "normal recovery completion no longer creates completed_early"
        );
        assert!(
            reopened
                .session_repo()
                .agent_completion_handoff(owner.id, recovery_turn)
                .expect("replacement completion handoff")
                .is_some(),
            "normal recovery completion publishes directly to its immediate parent"
        );
    }

    #[tokio::test]
    async fn orphan_crash_explicit_failure_and_interruption_resolve_deferred_receipt() {
        let (store, owner, crashed_turn, explicit) =
            orphan_crash_explicit_fixture("failed_recovery").await;
        let repository = store.session_repo();
        let failed_turn = TurnId::new();
        let failed_admission = repository
            .admit_agent_triggered_turn(owner.id, failed_turn, explicit.history_item_id)
            .await
            .expect("failed explicit recovery admission")
            .expect("failed explicit recovery admitted");
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    failed_admission.admission_id,
                    &failed_terminal(owner.id, "explicit recovery failed"),
                    failed_turn,
                    None,
                    None,
                )
                .await
                .expect("failed explicit recovery terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("superseded orphan crash")
                .deferred
                .expect("orphan crash deferred")
                .state,
            DeferredAgentCompletionState::Superseded
        );
        assert!(
            repository
                .agent_completion_handoff(owner.id, failed_turn)
                .expect("failed explicit recovery handoff")
                .is_some()
        );
        assert!(
            repository
                .pending_deferred_completion(owner.id)
                .expect("failed explicit recovery deferred")
                .is_none()
        );

        let (store, owner, crashed_turn, explicit) =
            orphan_crash_explicit_fixture("stopped_recovery").await;
        let repository = store.session_repo();
        let stopped_turn = TurnId::new();
        let stopped_admission = repository
            .admit_agent_triggered_turn(owner.id, stopped_turn, explicit.history_item_id)
            .await
            .expect("stopped explicit recovery admission")
            .expect("stopped explicit recovery admitted");
        let stopped = RunEvent::TurnTerminal {
            session_id: owner.id,
            terminal: Box::new(pre_admission_agent_interrupted_terminal()),
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    stopped_admission.admission_id,
                    &stopped,
                    stopped_turn,
                    None,
                    None,
                )
                .await
                .expect("stopped explicit recovery terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, crashed_turn)
                .expect("discarded orphan crash")
                .deferred
                .expect("orphan crash deferred")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            repository
                .agent_completion_handoff(owner.id, stopped_turn)
                .expect("stopped explicit recovery handoff")
                .is_none()
        );
        assert!(
            repository
                .pending_deferred_completion(owner.id)
                .expect("stopped explicit recovery deferred")
                .is_none()
        );
    }

    #[tokio::test]
    async fn pending_agent_trigger_exact_admission_remains_claimed_after_store_restart() {
        let (store, root_session_id) = test_repo().await;
        let (child, trigger_history_item_id, _) =
            spawn_pending_child(&store, root_session_id, "exact").await;
        let child_turn_id = TurnId::new();

        let admission = store
            .session_repo()
            .admit_agent_triggered_turn(child.id, child_turn_id, trigger_history_item_id)
            .await
            .expect("exact trigger admission")
            .expect("pending trigger admitted");
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger query"),
            Some(trigger_history_item_id),
            "admission owns execution, but the mailbox row remains pending until the safe context boundary"
        );

        let paths = store.paths().clone();
        drop(store);
        let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
        reopened_sqlite.migrate().expect("migrate reopened store");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("reopened pending trigger query"),
            Some(trigger_history_item_id)
        );
        let descendants = reopened
            .protocol_event_store()
            .retained_descendant_page(root_session_id, 0, 10)
            .expect("restarted descendant projection");
        assert_eq!(descendants.items.len(), 1);
        assert_eq!(
            descendants.items[0].pending_trigger_history_item_id,
            Some(trigger_history_item_id)
        );
        assert_eq!(
            reopened
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    child.id,
                    admission.admission_id,
                    child_turn_id,
                    128,
                )
                .expect("restarted safe trigger delivery")
                .history_item_ids,
            vec![trigger_history_item_id]
        );
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("delivered trigger query"),
            None
        );
    }

    #[tokio::test]
    async fn admitted_explicit_wake_abort_settles_only_its_claim_and_survives_reopen() {
        let (store, root_session_id) = test_repo().await;
        let (child, first_history_item_id, _) =
            spawn_pending_child(&store, root_session_id, "exact_abort").await;
        let repository = store.session_repo();
        let second = repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/exact_abort".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/exact_abort",
                        "/root",
                        "later independent wake",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("later explicit wake");
        let turn_id = TurnId::new();
        let admission = repository
            .admit_agent_triggered_turn(child.id, turn_id, first_history_item_id)
            .await
            .expect("explicit admission")
            .expect("explicit wake admitted");

        let terminal = pre_admission_agent_interrupted_terminal();
        assert!(matches!(
            repository
                .settle_agent_execution_wake_with_terminal(
                    child.id,
                    AgentExecutionWakeTerminalOwner::ExplicitTask(first_history_item_id),
                    terminal.clone(),
                )
                .expect("exact wake settlement"),
            AgentExecutionWakeTerminalSettlement::Applied {
                turn_id: observed_turn_id,
                terminal: observed_terminal,
            } if observed_turn_id == turn_id
                && observed_terminal.outcome == terminal.outcome
        ));
        assert!(matches!(
            repository
                .settle_agent_execution_wake_with_terminal(
                    child.id,
                    AgentExecutionWakeTerminalOwner::ExplicitTask(first_history_item_id),
                    terminal.clone(),
                )
                .expect("idempotent exact wake settlement"),
            AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
                turn_id: observed_turn_id,
                terminal: observed_terminal,
            } if observed_turn_id == turn_id
                && observed_terminal.outcome == terminal.outcome
        ));

        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            assert_eq!(
                connection
                    .query_row(
                        "SELECT admission_id, turn_id
                         FROM agent_trigger_turn_claims
                         WHERE history_item_id = ?1",
                        [first_history_item_id.to_string()],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .expect("durable wake claim"),
                (admission.admission_id.to_string(), turn_id.to_string())
            );
            let mut statement = connection
                .prepare(
                    "SELECT id, state
                     FROM agent_mailbox_messages
                     WHERE id IN (?1, ?2)
                     ORDER BY CASE id WHEN ?1 THEN 0 ELSE 1 END",
                )
                .expect("mailbox states");
            let states = statement
                .query_map(
                    params![
                        first_history_item_id.to_string(),
                        second.history_item_id.to_string()
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .expect("mailbox state rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("mailbox states");
            assert_eq!(
                states,
                vec![
                    (first_history_item_id.to_string(), "discarded".to_string()),
                    (second.history_item_id.to_string(), "pending".to_string()),
                ]
            );
            assert_eq!(
                connection
                    .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("tree fence count"),
                0,
                "an exact task abort must not create a subtree fence"
            );
        }

        let paths = store.paths().clone();
        drop(store);
        let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
        reopened_sqlite
            .migrate()
            .expect("validate reopened V53 storage");
        let reopened = StoreBundle::new(reopened_sqlite);
        let reopened_terminal = reopened
            .session_repo()
            .durable_terminal_for_turn(child.id, turn_id)
            .await
            .expect("reopened terminal")
            .expect("reopened exact terminal");
        assert_eq!(reopened_terminal.outcome, terminal.outcome);
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("later wake remains pending after reopen"),
            Some(second.history_item_id)
        );
    }

    #[tokio::test]
    async fn explicit_wake_abort_reports_an_existing_terminal_instead_of_rewriting_it() {
        let (store, root_session_id) = test_repo().await;
        let (child, history_item_id, _) =
            spawn_pending_child(&store, root_session_id, "terminal_wins").await;
        let repository = store.session_repo();
        let turn_id = TurnId::new();
        let admission = repository
            .admit_agent_triggered_turn(child.id, turn_id, history_item_id)
            .await
            .expect("explicit admission")
            .expect("explicit wake admitted");
        let failed = pre_admission_failed_terminal("durable failure won first");
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    admission.admission_id,
                    &RunEvent::TurnTerminal {
                        session_id: child.id,
                        terminal: Box::new(failed.clone()),
                    },
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("durable failure"),
            AdmittedTerminalCommit::Applied
        );

        assert!(matches!(
            repository
                .settle_agent_execution_wake_with_terminal(
                    child.id,
                    AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id),
                    pre_admission_agent_interrupted_terminal(),
                )
                .expect("late hard-abort settlement"),
            AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
                turn_id: observed_turn_id,
                terminal: observed_terminal,
            } if observed_turn_id == turn_id
                && observed_terminal.outcome == failed.outcome
        ));
    }

    #[tokio::test]
    async fn admitted_explicit_wake_terminal_resolves_only_its_claim_before_safe_delivery() {
        let cases = [
            (
                "completed",
                DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Completed,
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                },
                "delivered",
            ),
            (
                "failed",
                pre_admission_failed_terminal("setup failed before mailbox delivery"),
                "delivered",
            ),
            (
                "interrupted",
                pre_admission_agent_interrupted_terminal(),
                "discarded",
            ),
        ];

        for (case, terminal, expected_first_state) in cases {
            let (store, root_session_id) = test_repo().await;
            let child_name = format!("resolve_{case}");
            let child_path = format!("/root/{child_name}");
            let (child, first_history_item_id, _) =
                spawn_pending_child(&store, root_session_id, &child_name).await;
            let repository = store.session_repo();
            let second = repository
                .append_inter_agent_communication_with_protocol_bundle(
                    child.id,
                    InterAgentCommunication {
                        author: "/root".to_string(),
                        recipient: child_path,
                        content: render_inter_agent_message(
                            InterAgentMessageType::NewTask,
                            &format!("/root/{child_name}"),
                            "/root",
                            "later independent wake",
                        ),
                        trigger_turn: true,
                    },
                    false,
                )
                .expect("later explicit wake");
            let turn_id = TurnId::new();
            let admission = repository
                .admit_agent_triggered_turn(child.id, turn_id, first_history_item_id)
                .await
                .expect("explicit admission")
                .expect("explicit wake admitted");
            let event = RunEvent::TurnTerminal {
                session_id: child.id,
                terminal: Box::new(terminal.clone()),
            };
            assert_eq!(
                repository
                    .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                        child.id,
                        admission.admission_id,
                        &event,
                        turn_id,
                        None,
                        false,
                        None,
                    )
                    .await
                    .expect("terminal before safe mailbox delivery"),
                AdmittedTerminalCommit::Applied,
                "{case}"
            );

            {
                let connection = repository.connection.lock().expect("sqlite mutex");
                let first = connection
                    .query_row(
                        "SELECT state, delivered_turn_id,
                                resolved_by_terminal_event_id
                         FROM agent_mailbox_messages
                         WHERE id = ?1",
                        [first_history_item_id.to_string()],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )
                    .expect("claimed wake state");
                assert_eq!(first.0, expected_first_state, "{case}");
                if expected_first_state == "delivered" {
                    assert_eq!(first.1, Some(turn_id.to_string()), "{case}");
                    assert_eq!(first.2, None, "{case}");
                } else {
                    assert_eq!(first.1, None, "{case}");
                    assert!(first.2.is_some(), "{case}");
                }
                assert_eq!(
                    connection
                        .query_row(
                            "SELECT state
                             FROM agent_mailbox_messages
                             WHERE id = ?1",
                            [second.history_item_id.to_string()],
                            |row| row.get::<_, String>(0),
                        )
                        .expect("later wake state"),
                    "pending",
                    "{case}"
                );
            }

            assert!(matches!(
                repository
                    .settle_agent_execution_wake_with_terminal(
                        child.id,
                        AgentExecutionWakeTerminalOwner::ExplicitTask(first_history_item_id),
                        pre_admission_agent_interrupted_terminal(),
                    )
                    .expect("idempotent wake settlement"),
                AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
                    turn_id: observed_turn_id,
                    terminal: observed_terminal,
                } if observed_turn_id == turn_id
                    && observed_terminal.outcome == terminal.outcome
            ));

            let paths = store.paths().clone();
            drop(repository);
            drop(store);
            let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
            reopened_sqlite
                .migrate()
                .expect("reopen validates exact V53 claim resolution");
            let reopened = StoreBundle::new(reopened_sqlite);
            assert_eq!(
                reopened
                    .session_repo()
                    .pending_agent_trigger_history_item_id(child.id)
                    .expect("later wake remains schedulable"),
                Some(second.history_item_id),
                "{case}"
            );
            assert!(
                reopened
                    .session_repo()
                    .admit_agent_triggered_turn(child.id, TurnId::new(), second.history_item_id,)
                    .await
                    .expect("later wake admission")
                    .is_some(),
                "{case}: later wake must not collide with the resolved claim"
            );
        }
    }

    #[tokio::test]
    async fn explicit_wake_admission_and_abort_are_atomic_across_connections() {
        let (store, root_session_id) = test_repo().await;
        let (child, history_item_id, _) =
            spawn_pending_child(&store, root_session_id, "claim_abort_race").await;
        let later = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/claim_abort_race".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/claim_abort_race",
                        "/root",
                        "later wake must survive either race winner",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("later explicit wake");
        let competing_sqlite = SqliteStore::open(store.paths()).expect("second sqlite connection");
        competing_sqlite
            .migrate()
            .expect("migrate second connection");
        let competing = StoreBundle::new(competing_sqlite);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let admitted_turn_id = TurnId::new();

        let (admission, settlement) = std::thread::scope(|scope| {
            let admission_barrier = barrier.clone();
            let admission_store = &store;
            let admission_thread = scope.spawn(move || {
                admission_barrier.wait();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("admission runtime")
                    .block_on(admission_store.session_repo().admit_agent_triggered_turn(
                        child.id,
                        admitted_turn_id,
                        history_item_id,
                    ))
            });
            let settlement_barrier = barrier.clone();
            let settlement_store = &competing;
            let settlement_thread = scope.spawn(move || {
                settlement_barrier.wait();
                settlement_store
                    .session_repo()
                    .settle_agent_execution_wake_with_terminal(
                        child.id,
                        AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id),
                        pre_admission_agent_interrupted_terminal(),
                    )
            });
            (
                admission_thread.join().expect("admission thread"),
                settlement_thread.join().expect("settlement thread"),
            )
        });
        let admission = admission.expect("admission result");
        let settlement = settlement.expect("settlement result");
        let settled_turn_id = match settlement {
            AgentExecutionWakeTerminalSettlement::Applied { turn_id, .. } => turn_id,
            other => panic!("abort must own the selected wake exactly once: {other:?}"),
        };
        match admission {
            Some(admission) => {
                assert_eq!(settled_turn_id, admitted_turn_id);
                let repository = store.session_repo();
                let connection = repository.connection.lock().expect("sqlite mutex");
                assert_eq!(
                    connection
                        .query_row(
                            "SELECT admission_id
                             FROM agent_trigger_turn_claims
                             WHERE history_item_id = ?1",
                            [history_item_id.to_string()],
                            |row| row.get::<_, String>(0),
                        )
                        .expect("winning admission claim"),
                    admission.admission_id.to_string()
                );
            }
            None => {
                assert_ne!(
                    settled_turn_id, admitted_turn_id,
                    "pre-admission abort owns a synthetic exact turn"
                );
            }
        }
        let terminal_events = store
            .protocol_event_store()
            .list_runtime_events_for_session(child.id)
            .expect("terminal events")
            .into_iter()
            .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
            .count();
        assert_eq!(terminal_events, 1);
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("later wake after race"),
            Some(later.history_item_id),
            "neither race winner may retire a later explicit wake"
        );

        let paths = store.paths().clone();
        drop(competing);
        drop(store);
        let reopened_sqlite = SqliteStore::open(&paths).expect("reopen race store");
        reopened_sqlite
            .migrate()
            .expect("reopen validates race winner claim");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("reopened later wake"),
            Some(later.history_item_id)
        );
        assert!(
            reopened
                .session_repo()
                .admit_agent_triggered_turn(child.id, TurnId::new(), later.history_item_id)
                .await
                .expect("reopened later wake admission")
                .is_some(),
            "the later wake must remain independently admissible after reopen"
        );
    }

    #[tokio::test]
    async fn pending_agent_trigger_old_lease_coalesces_newer_session_mail_into_one_turn() {
        let (store, root_session_id) = test_repo().await;
        let (child, first_trigger_history_item_id, _) =
            spawn_pending_child(&store, root_session_id, "coalesced").await;
        let second_trigger = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/coalesced".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/coalesced",
                        "/root",
                        "include this follow-up in the same pending batch",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("newer pending trigger");
        assert!(second_trigger.schedule_turn);
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("oldest schedulable pending trigger"),
            Some(first_trigger_history_item_id)
        );

        // One admitted turn consumes all session-scoped mail that precedes its
        // SessionStarted append. Keeping the older exact lease valid here avoids
        // losing or repeatedly rescheduling the already-reserved execution when
        // another message arrives before admission.
        let child_turn_id = TurnId::new();
        let admission = store
            .session_repo()
            .admit_agent_triggered_turn(child.id, child_turn_id, first_trigger_history_item_id)
            .await
            .expect("coalesced exact trigger admission")
            .expect("older reserved trigger remains admissible");
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger before safe delivery"),
            Some(first_trigger_history_item_id)
        );
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    child.id,
                    admission.admission_id,
                    child_turn_id,
                    128,
                )
                .expect("coalesced safe mailbox delivery")
                .history_item_ids,
            vec![
                first_trigger_history_item_id,
                second_trigger.history_item_id
            ]
        );
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger after safe delivery"),
            None
        );
        assert!(
            store
                .session_repo()
                .admit_agent_triggered_turn(child.id, TurnId::new(), second_trigger.history_item_id)
                .await
                .expect("newer trigger retry")
                .is_none(),
            "the same SessionStarted append must claim both pending triggers"
        );
        let events = store
            .protocol_event_store()
            .list_runtime_events(child.id, child_turn_id)
            .expect("coalesced turn events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.msg, RuntimeEventMsg::Warning { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn pending_agent_trigger_synthetic_failed_commits_terminal_final_and_receipt_once() {
        let (store, root_session_id) = test_repo().await;
        let (child, trigger_history_item_id, root_turn_id) =
            spawn_pending_child(&store, root_session_id, "failed").await;
        let terminal = pre_admission_failed_terminal("launch failed before admission");

        let (child_turn_id, handoff) = match store
            .session_repo()
            .settle_pending_agent_trigger_with_terminal(
                child.id,
                trigger_history_item_id,
                terminal.clone(),
            )
            .expect("synthetic failure settlement")
        {
            PendingAgentTriggerSettlement::Applied { turn_id, handoff } => {
                (turn_id, handoff.expect("failed child handoff"))
            }
            other => panic!("pending trigger was unexpectedly unavailable: {other:?}"),
        };
        assert_eq!(stored_admission_state(&store, child.id).0, "failed");
        let child_events = store
            .protocol_event_store()
            .list_runtime_events(child.id, child_turn_id)
            .expect("synthetic child events");
        assert_eq!(
            child_events
                .iter()
                .filter(|event| matches!(
                    &event.msg,
                    RuntimeEventMsg::Warning { message }
                        if message.starts_with("thread started:")
                ))
                .count(),
            1
        );
        assert_eq!(
            child_events
                .iter()
                .filter(|event| matches!(
                    &event.msg,
                    RuntimeEventMsg::TurnTerminal { terminal: stored }
                        if matches!(
                            &stored.outcome,
                            TurnTerminalOutcome::Failed { error }
                                if error == "launch failed before admission"
                        )
                            && stored.final_response_id.is_none()
                            && stored.tool_call_count == 0
                            && stored.failed_tool_count == 0
                            && stored.change_count == 0
                ))
                .count(),
            1
        );
        assert_eq!(handoff.child_session_id, child.id);
        assert_eq!(handoff.child_turn_id, child_turn_id);
        assert_eq!(handoff.parent_session_id, root_session_id);
        assert_eq!(
            store
                .session_repo()
                .agent_completion_handoff(child.id, child_turn_id)
                .expect("completion receipt query"),
            Some(handoff.clone())
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(root_session_id, root_turn_id)
                .expect("parent history before safe delivery")
                .into_iter()
                .all(|item| item.id != handoff.history_item_id)
        );
        let parent_finals = store
            .session_repo()
            .agent_mailbox_communications_by_id(root_session_id, &[handoff.history_item_id])
            .expect("queued parent FINAL");
        assert!(matches!(
            parent_finals.as_slice(),
            [(id, communication)]
                if *id == handoff.history_item_id
                    && communication.author == "/root/failed"
                    && communication.recipient == "/root"
                    && !communication.trigger_turn
        ));

        assert_eq!(
            store
                .session_repo()
                .settle_pending_agent_trigger_with_terminal(
                    child.id,
                    trigger_history_item_id,
                    terminal,
                )
                .expect("idempotent synthetic failure retry"),
            PendingAgentTriggerSettlement::WakeOwnedOrResolved
        );
        assert_eq!(
            store
                .protocol_event_store()
                .list_runtime_events(child.id, child_turn_id)
                .expect("events after retry")
                .len(),
            child_events.len()
        );
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM agent_completion_handoffs
                     WHERE child_session_id = ?1 AND child_turn_id = ?2",
                    params![child.id.to_string(), child_turn_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("completion receipt count"),
            1
        );
    }

    #[tokio::test]
    async fn synthetic_failed_terminal_delivers_only_its_exact_pending_trigger() {
        let (store, root_session_id) = test_repo().await;
        let (child, first_trigger_history_item_id, _root_turn_id) =
            spawn_pending_child(&store, root_session_id, "exact_synthetic").await;
        let repository = store.session_repo();
        let second = repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/exact_synthetic".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/exact_synthetic",
                        "/root",
                        "later independent trigger",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("second pending trigger");

        let synthetic_turn_id = match repository
            .settle_pending_agent_trigger_with_terminal(
                child.id,
                first_trigger_history_item_id,
                pre_admission_failed_terminal("first launch failed"),
            )
            .expect("exact synthetic terminal")
        {
            PendingAgentTriggerSettlement::Applied {
                turn_id,
                handoff: Some(_),
            } => turn_id,
            other => panic!("synthetic failure was unexpectedly unavailable: {other:?}"),
        };

        let mailbox_states = {
            let connection = repository.connection.lock().expect("sqlite mutex");
            let mut statement = connection
                .prepare(
                    "SELECT id, state, delivered_turn_id
                 FROM agent_mailbox_messages
                 WHERE recipient_session_id = ?1
                   AND id IN (?2, ?3)
                 ORDER BY CASE id WHEN ?2 THEN 0 ELSE 1 END",
                )
                .expect("mailbox state query");
            statement
                .query_map(
                    params![
                        child.id.to_string(),
                        first_trigger_history_item_id.to_string(),
                        second.history_item_id.to_string(),
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .expect("mailbox state rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("mailbox states after exact settlement")
        };
        assert_eq!(
            mailbox_states,
            vec![
                (
                    first_trigger_history_item_id.to_string(),
                    "delivered".to_string(),
                    Some(synthetic_turn_id.to_string()),
                ),
                (
                    second.history_item_id.to_string(),
                    "pending".to_string(),
                    None,
                )
            ]
        );
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(child.id)
                .expect("later trigger remains schedulable"),
            Some(second.history_item_id)
        );
        let retained = store
            .protocol_event_store()
            .retained_descendant_snapshot(root_session_id, 1)
            .expect("fresh retained projection after exact settlement");
        assert_eq!(
            retained[0].pending_trigger_history_item_id,
            Some(second.history_item_id)
        );
        assert!(retained[0].pending_trigger_schedule_ready);
        assert!(
            repository
                .admit_agent_triggered_turn(child.id, TurnId::new(), second.history_item_id)
                .await
                .expect("later exact trigger admission")
                .is_some(),
            "the untouched later trigger must remain an admissible scheduler owner"
        );
    }

    #[tokio::test]
    async fn pending_agent_trigger_synthetic_interrupted_does_not_create_handoff() {
        let (store, root_session_id) = test_repo().await;
        let (child, trigger_history_item_id, root_turn_id) =
            spawn_pending_child(&store, root_session_id, "interrupted").await;
        let terminal = pre_admission_interrupted_terminal();

        let child_turn_id = match store
            .session_repo()
            .settle_pending_agent_trigger_with_terminal(
                child.id,
                trigger_history_item_id,
                terminal.clone(),
            )
            .expect("synthetic interrupted settlement")
        {
            PendingAgentTriggerSettlement::Applied { turn_id, handoff } => {
                assert_eq!(handoff, None);
                turn_id
            }
            other => panic!("pending trigger was unexpectedly unavailable: {other:?}"),
        };
        assert_eq!(stored_admission_state(&store, child.id).0, "cancelled");
        assert!(matches!(
            store
                .protocol_event_store()
                .list_runtime_events(child.id, child_turn_id)
                .expect("interrupted child events")
                .as_slice(),
            [RuntimeEvent {
                msg: RuntimeEventMsg::Warning { .. },
                ..
            }, RuntimeEvent {
                msg: RuntimeEventMsg::TurnTerminal { terminal: stored },
                ..
            }] if matches!(
                stored.outcome,
                TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::TreeStopped
                }
            )
                && stored.final_response_id.is_none()
                && stored.tool_call_count == 0
                && stored.failed_tool_count == 0
                && stored.change_count == 0
        ));
        assert!(
            store
                .session_repo()
                .agent_completion_handoff(child.id, child_turn_id)
                .expect("interrupted receipt query")
                .is_none()
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(root_session_id, root_turn_id)
                .expect("parent history")
                .into_iter()
                .all(|item| !matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                ))
        );
    }

    #[tokio::test]
    async fn pending_agent_trigger_receipt_insert_failure_rolls_back_and_remains_pending() {
        let (store, root_session_id) = test_repo().await;
        let (child, trigger_history_item_id, root_turn_id) =
            spawn_pending_child(&store, root_session_id, "rollback").await;
        let repository = store.session_repo();
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TEMP TRIGGER abort_pending_agent_completion_receipt
                 BEFORE INSERT ON agent_completion_handoffs
                 BEGIN
                     SELECT RAISE(ABORT, 'injected pending completion receipt failure');
                 END;",
            )
            .expect("install receipt failure");

        let error = repository
            .settle_pending_agent_trigger_with_terminal(
                child.id,
                trigger_history_item_id,
                pre_admission_failed_terminal("must roll back"),
            )
            .expect_err("receipt failure must roll back the whole settlement");
        assert!(
            error
                .to_string()
                .contains("injected pending completion receipt failure")
        );
        assert_eq!(
            stored_admission_state(&store, child.id),
            ("idle".to_string(), None, None, None)
        );
        assert!(
            store
                .protocol_event_store()
                .list_runtime_events_for_session(child.id)
                .expect("child events after rollback")
                .is_empty()
        );
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger after rollback"),
            Some(trigger_history_item_id)
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(root_session_id, root_turn_id)
                .expect("parent history after rollback")
                .into_iter()
                .all(|item| !matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                ))
        );
        assert_eq!(
            repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .query_row(
                    "SELECT COUNT(*) FROM agent_completion_handoffs",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("receipt count after rollback"),
            0
        );

        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch("DROP TRIGGER abort_pending_agent_completion_receipt;")
            .expect("drop receipt failure");
        assert!(matches!(
            repository
                .settle_pending_agent_trigger_with_terminal(
                    child.id,
                    trigger_history_item_id,
                    pre_admission_failed_terminal("retry succeeds"),
                )
                .expect("retry settlement"),
            PendingAgentTriggerSettlement::Applied {
                handoff: Some(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn pending_agent_trigger_admission_and_synthetic_failure_have_one_durable_winner() {
        let (store, root_session_id) = test_repo().await;
        let (child, trigger_history_item_id, _root_turn_id) =
            spawn_pending_child(&store, root_session_id, "raced").await;
        let reopened_sqlite = SqliteStore::open(store.paths()).expect("second sqlite connection");
        reopened_sqlite
            .migrate()
            .expect("migrate second connection");
        let competing_store = StoreBundle::new(reopened_sqlite);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let admitted_turn_id = TurnId::new();

        let (admission, settlement) = std::thread::scope(|scope| {
            let admission_store = &store;
            let settlement_store = &competing_store;
            let child_session_id = child.id;
            let admission_barrier = barrier.clone();
            let admission_thread = scope.spawn(move || {
                admission_barrier.wait();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("admission runtime")
                    .block_on(admission_store.session_repo().admit_agent_triggered_turn(
                        child_session_id,
                        admitted_turn_id,
                        trigger_history_item_id,
                    ))
            });
            let settlement_barrier = barrier.clone();
            let settlement_thread = scope.spawn(move || {
                settlement_barrier.wait();
                settlement_store
                    .session_repo()
                    .settle_pending_agent_trigger_with_terminal(
                        child_session_id,
                        trigger_history_item_id,
                        pre_admission_failed_terminal("pre-admission owner lost the race"),
                    )
            });
            (
                admission_thread.join().expect("admission thread"),
                settlement_thread.join().expect("settlement thread"),
            )
        });
        let admission = admission.expect("admission result");
        let settlement = settlement.expect("settlement result");

        match (admission, settlement) {
            (Some(_snapshot), PendingAgentTriggerSettlement::WakeOwnedOrResolved) => {
                let state = stored_admission_state(&store, child.id);
                assert_eq!(state.0, "running");
                assert_eq!(state.2, Some(admitted_turn_id.to_string()));
                assert!(
                    store
                        .session_repo()
                        .agent_completion_handoff(child.id, admitted_turn_id)
                        .expect("winning admission handoff query")
                        .is_none()
                );
            }
            (
                None,
                PendingAgentTriggerSettlement::Applied {
                    turn_id: synthetic_turn_id,
                    handoff: Some(handoff),
                },
            ) => {
                assert_eq!(stored_admission_state(&store, child.id).0, "failed");
                assert_eq!(handoff.child_turn_id, synthetic_turn_id);
                assert_eq!(handoff.parent_session_id, root_session_id);
            }
            other => {
                panic!("trigger race produced more or fewer than one durable winner: {other:?}")
            }
        }
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger after race"),
            None
        );
        assert_eq!(
            store
                .protocol_event_store()
                .list_runtime_events_for_session(child.id)
                .expect("runtime events after race")
                .into_iter()
                .filter(|event| {
                    matches!(
                        &event.msg,
                        RuntimeEventMsg::Warning { message }
                            if message.starts_with("thread started:")
                    )
                })
                .count(),
            1,
            "admission and settlement must share one append-order claim boundary"
        );
    }

    #[tokio::test]
    async fn owner_resume_admission_coalesces_all_pending_rows_and_terminal_resolves_them() {
        let (store, root_session_id) = test_repo().await;
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 2).await;
        let owner_turn_id = TurnId::new();
        let admission = store
            .session_repo()
            .admit_owner_resume_turn(owner.id, owner_turn_id, requests[0].request_id)
            .await
            .expect("owner-resume admission")
            .expect("owner resume admitted");
        assert!(
            store
                .session_repo()
                .list_pending_owner_resume_requests(owner.id)
                .expect("pending requests after claim")
                .is_empty()
        );
        {
            let repository = store.session_repo();
            let connection = repository.connection.lock().expect("sqlite mutex");
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*)
                         FROM agent_owner_resume_requests
                         WHERE owner_session_id = ?1
                           AND state = 'claimed'
                           AND claimed_turn_id = ?2",
                        params![owner.id.to_string(), owner_turn_id.to_string()],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("claimed resume count"),
                2
            );
        }
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    admission.admission_id,
                    owner_turn_id,
                    128,
                )
                .expect("safe coalesced OwnerResume delivery")
                .history_item_ids,
            requests
                .iter()
                .map(|request| request.request_id.0)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    admission.admission_id,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn_id,
                    None,
                    None,
                )
                .await
                .expect("owner terminal"),
            AdmittedTerminalCommit::Applied
        );
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1
                       AND state = 'resolved'
                       AND claimed_turn_id = ?2
                       AND resolved_at_ms IS NOT NULL",
                    params![owner.id.to_string(), owner_turn_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("resolved resume count"),
            2
        );
    }

    #[tokio::test]
    async fn admitted_owner_resume_abort_uses_the_claimed_turn_and_is_idempotent() {
        let (store, root_session_id) = test_repo().await;
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 2).await;
        let repository = store.session_repo();
        let turn_id = TurnId::new();
        repository
            .admit_owner_resume_turn(owner.id, turn_id, requests[0].request_id)
            .await
            .expect("owner-resume admission")
            .expect("owner resume admitted");
        let terminal = pre_admission_agent_interrupted_terminal();

        assert!(matches!(
            repository
                .settle_agent_execution_wake_with_terminal(
                    owner.id,
                    AgentExecutionWakeTerminalOwner::OwnerResume(requests[0].request_id),
                    terminal.clone(),
                )
                .expect("owner-resume hard abort"),
            AgentExecutionWakeTerminalSettlement::Applied {
                turn_id: observed_turn_id,
                terminal: observed_terminal,
            } if observed_turn_id == turn_id
                && observed_terminal.outcome == terminal.outcome
        ));
        assert!(matches!(
            repository
                .settle_agent_execution_wake_with_terminal(
                    owner.id,
                    AgentExecutionWakeTerminalOwner::OwnerResume(requests[0].request_id),
                    terminal.clone(),
                )
                .expect("idempotent owner-resume hard abort"),
            AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
                turn_id: observed_turn_id,
                terminal: observed_terminal,
            } if observed_turn_id == turn_id
                && observed_terminal.outcome == terminal.outcome
        ));
        let connection = repository.connection.lock().expect("sqlite mutex");
        let owner_resume_rows = connection
            .prepare(
                "SELECT state, claimed_turn_id
                 FROM agent_owner_resume_requests
                 WHERE owner_session_id = ?1
                 ORDER BY source_history_item_id",
            )
            .expect("prepare owner-resume state query")
            .query_map(params![owner.id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .expect("query owner-resume states")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect owner-resume states");
        assert_eq!(
            owner_resume_rows,
            vec![
                ("resolved".to_string(), Some(turn_id.to_string())),
                ("resolved".to_string(), Some(turn_id.to_string())),
            ]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("tree fence count"),
            0
        );
    }

    #[tokio::test]
    async fn owner_resume_state_trigger_prevents_claim_aba_and_resolved_reopen() {
        let (store, root_session_id) = test_repo().await;
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        {
            let repository = store.session_repo();
            let connection = repository.connection.lock().expect("sqlite mutex");
            assert!(
                connection
                    .execute(
                        "INSERT INTO agent_owner_resume_requests
                         (owner_session_id, source_session_id, source_history_item_id,
                          state, claimed_turn_id, created_at_ms, updated_at_ms,
                          claimed_at_ms, resolved_at_ms)
                         VALUES (?1, ?2, ?3, 'pending', NULL, 1, 1, NULL, NULL)",
                        params![
                            root_session_id.to_string(),
                            owner.id.to_string(),
                            requests[0].request_id.to_string()
                        ],
                    )
                    .is_err(),
                "root cannot own an OwnerResume request"
            );
        }
        let turn_id = TurnId::new();
        let admission = store
            .session_repo()
            .admit_owner_resume_turn(owner.id, turn_id, requests[0].request_id)
            .await
            .expect("owner-resume admission")
            .expect("owner resume admitted");
        {
            let repository = store.session_repo();
            let connection = repository.connection.lock().expect("sqlite mutex");
            assert!(
                connection
                    .execute(
                        "UPDATE agent_owner_resume_requests
                         SET state = 'resolved',
                             claimed_turn_id = ?3,
                             claimed_at_ms = claimed_at_ms + 1,
                             resolved_at_ms = updated_at_ms + 1,
                             updated_at_ms = updated_at_ms + 1
                         WHERE owner_session_id = ?1
                           AND source_history_item_id = ?2",
                        params![
                            owner.id.to_string(),
                            requests[0].request_id.to_string(),
                            TurnId::new().to_string()
                        ],
                    )
                    .is_err(),
                "claimed identity cannot change while resolving"
            );
        }
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    admission.admission_id,
                    turn_id,
                    128,
                )
                .expect("safe OwnerResume delivery")
                .history_item_ids,
            vec![requests[0].request_id.0]
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    admission.admission_id,
                    &completed_terminal_for_response(owner.id, None),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("owner terminal"),
            AdmittedTerminalCommit::Applied
        );
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert!(
            connection
                .execute(
                    "UPDATE agent_owner_resume_requests
                     SET state = 'pending',
                         claimed_turn_id = NULL,
                         claimed_at_ms = NULL,
                         resolved_at_ms = NULL,
                         updated_at_ms = updated_at_ms + 1
                     WHERE owner_session_id = ?1
                       AND source_history_item_id = ?2",
                    params![owner.id.to_string(), requests[0].request_id.to_string()],
                )
                .is_err(),
            "resolved owner-resume requests are terminal"
        );
    }

    #[tokio::test]
    #[cfg(any())]
    async fn fresh_nested_followup_schedules_inactive_immediate_owner_without_restart() {
        let (store, root_session_id) = test_repo().await;
        let owner = create_sibling_session(&store, root_session_id, "owner").await;
        let nested = create_sibling_session(&store, root_session_id, "nested").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                owner.id,
                "/root/owner",
                "owner",
            )
            .await
            .expect("owner edge");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                nested.id,
                "/root/owner/nested",
                "nested",
            )
            .await
            .expect("nested edge");
        let _ = active_turn(&store, root_session_id).await;

        let stored = repository
            .append_inter_agent_communication_with_protocol_bundle(
                nested.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner/nested".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner/nested",
                        "/root",
                        "fresh nested follow-up",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("nested follow-up");

        assert!(stored.schedule_turn);
        assert_eq!(
            stored.scheduled_owner_resumes,
            vec![ScheduledOwnerResume {
                owner_session_id: owner.id,
                request_id: OwnerResumeRequestId::from(stored.history_item_id),
            }]
        );
        assert_eq!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("live owner-resume projection"),
            Some(OwnerResumeRequestId::from(stored.history_item_id))
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(root_session_id)
                .expect("root resume rows")
                .is_empty()
        );
    }

    #[tokio::test]
    #[cfg(any())]
    async fn fresh_owner_blocks_nested_resume_but_expired_owner_does_not() {
        let (store, root_session_id) = test_repo().await;
        let owner = create_sibling_session(&store, root_session_id, "owner").await;
        let nested = create_sibling_session(&store, root_session_id, "nested").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                owner.id,
                "/root/owner",
                "owner",
            )
            .await
            .expect("owner edge");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                nested.id,
                "/root/owner/nested",
                "nested",
            )
            .await
            .expect("nested edge");
        let (owner_admission_id, owner_turn_id) = active_turn(&store, owner.id).await;
        let fresh = repository
            .append_inter_agent_communication_with_protocol_bundle(
                nested.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner/nested".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner/nested",
                        "/root",
                        "fresh-owner follow-up",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("fresh-owner nested trigger");
        assert!(fresh.scheduled_owner_resumes.is_empty());
        assert!(
            repository
                .list_pending_owner_resume_requests(owner.id)
                .expect("fresh owner requests")
                .is_empty()
        );

        repository
            .inject_raw_runtime_state_for_corruption_test(
                owner.id,
                "running",
                Some(&owner_admission_id.to_string()),
                Some(&owner_turn_id.to_string()),
                Some(1),
            )
            .expect("expire owner admission");
        let expired = repository
            .append_inter_agent_communication_with_protocol_bundle(
                nested.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner/nested".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner/nested",
                        "/root",
                        "expired-owner follow-up",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("expired-owner nested trigger");
        assert_eq!(expired.scheduled_owner_resumes.len(), 1);
        assert_eq!(
            expired.scheduled_owner_resumes[0].owner_session_id,
            owner.id
        );
    }

    #[tokio::test]
    #[cfg(any())]
    async fn corrupt_ancestor_admission_rolls_back_nested_trigger_and_resume_schedule() {
        let (store, root_session_id) = test_repo().await;
        let owner = create_sibling_session(&store, root_session_id, "owner").await;
        let nested = create_sibling_session(&store, root_session_id, "nested").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                owner.id,
                "/root/owner",
                "owner",
            )
            .await
            .expect("owner edge");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                nested.id,
                "/root/owner/nested",
                "nested",
            )
            .await
            .expect("nested edge");
        repository
            .inject_raw_runtime_state_for_corruption_test(
                owner.id,
                "running",
                Some(&AdmissionId::new().to_string()),
                None,
                Some(i64::MAX - 1),
            )
            .expect("partial owner admission");
        let history_before = store
            .protocol_event_store()
            .list_history_items_for_session(nested.id)
            .expect("history before corruption append")
            .len();

        let error = repository
            .append_inter_agent_communication_with_protocol_bundle(
                nested.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner/nested".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner/nested",
                        "/root",
                        "must roll back",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect_err("partial ancestor admission must fail closed");
        assert!(error.to_string().contains("partial durable run admission"));
        assert_eq!(
            store
                .protocol_event_store()
                .list_history_items_for_session(nested.id)
                .expect("history after rolled-back append")
                .len(),
            history_before
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(owner.id)
                .expect("requests after rolled-back append")
                .is_empty()
        );
    }

    #[tokio::test]
    #[cfg(any())]
    async fn lazy_upgrade_seed_restores_idle_nested_trigger_and_is_idempotent() {
        let (store, root_session_id) = test_repo().await;
        let owner = create_sibling_session(&store, root_session_id, "owner").await;
        let nested = create_sibling_session(&store, root_session_id, "nested").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                owner.id,
                "/root/owner",
                "owner",
            )
            .await
            .expect("owner edge");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                nested.id,
                "/root/owner/nested",
                "nested",
            )
            .await
            .expect("nested edge");
        let trigger = repository
            .append_inter_agent_communication_with_protocol_bundle(
                nested.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner/nested".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner/nested",
                        "/root",
                        "pre-V48 pending nested work",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("nested trigger");
        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            connection
                .execute("DELETE FROM agent_owner_resume_requests", [])
                .expect("simulate empty V48 upgrade table");
        }

        let seeded = repository
            .ensure_owner_resumes_for_pending_nested_triggers(root_session_id)
            .expect("lazy V48 seed");
        assert_eq!(
            seeded,
            vec![ScheduledOwnerResume {
                owner_session_id: owner.id,
                request_id: OwnerResumeRequestId::from(trigger.history_item_id),
            }]
        );
        assert_eq!(
            repository
                .ensure_owner_resumes_for_pending_nested_triggers(root_session_id)
                .expect("idempotent lazy V48 seed"),
            seeded
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(root_session_id)
                .expect("root resume requests")
                .is_empty()
        );
    }

    #[tokio::test]
    #[cfg(any())]
    async fn newer_nested_followup_projects_the_owners_canonical_earliest_pending_resume() {
        let (store, root_session_id) = test_repo().await;
        let (owner, existing_requests) =
            nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let nested = create_sibling_session(&store, root_session_id, "new_nested").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                nested.id,
                "/root/owner/new_nested",
                "new_nested",
            )
            .await
            .expect("new nested edge");

        let stored = repository
            .append_inter_agent_communication_with_protocol_bundle(
                nested.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner/new_nested".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner/new_nested",
                        "/root",
                        "newer nested follow-up",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("newer nested follow-up");

        assert_eq!(stored.scheduled_owner_resumes.len(), 1);
        assert_eq!(stored.scheduled_owner_resumes[0].owner_session_id, owner.id);
        assert_eq!(
            stored.scheduled_owner_resumes[0].request_id, existing_requests[0].request_id,
            "live projection must reserve the canonical oldest pending request"
        );
        assert_ne!(
            stored.scheduled_owner_resumes[0].request_id.to_string(),
            stored.history_item_id.to_string()
        );
        assert_eq!(
            repository
                .list_pending_owner_resume_requests(owner.id)
                .expect("coalesced pending rows")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn newer_completion_handoff_projects_the_parents_canonical_pending_resume() {
        let (store, root_session_id) = test_repo().await;
        let (owner, existing_requests) =
            nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let child = create_sibling_session(&store, root_session_id, "later_child").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                child.id,
                "/root/owner/later_child",
                "later_child",
            )
            .await
            .expect("later child edge");
        let (admission_id, turn_id) = active_turn(&store, child.id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    admission_id,
                    &failed_terminal(child.id, "later child failed"),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("later child terminal"),
            AdmittedTerminalCommit::Applied
        );

        let handoff = repository
            .agent_completion_handoff(child.id, turn_id)
            .expect("later child handoff")
            .expect("failed child FINAL");
        assert_ne!(
            handoff.history_item_id.to_string(),
            existing_requests[0].request_id.to_string()
        );
        assert_eq!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("current handoff owner"),
            Some(existing_requests[0].request_id)
        );
        assert_eq!(
            repository
                .list_pending_owner_resume_requests(owner.id)
                .expect("pending handoff requests")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn late_child_handoff_is_preserved_for_failed_parent_without_owner_resume() {
        let (store, root_session_id) = test_repo().await;
        let owner = create_sibling_session(&store, root_session_id, "owner").await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                owner.id,
                "/root/owner",
                "owner",
            )
            .await
            .expect("owner edge");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                child.id,
                "/root/owner/child",
                "child",
            )
            .await
            .expect("child edge");
        let (owner_admission_id, owner_turn_id) = active_turn(&store, owner.id).await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission_id,
                    &failed_terminal(owner.id, "owner failed explicitly"),
                    owner_turn_id,
                    None,
                    None,
                )
                .await
                .expect("owner failure"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, None),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("late child completion"),
            AdmittedTerminalCommit::Applied
        );

        let handoff = repository
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("late child handoff")
            .expect("late child FINAL is retained");
        assert_eq!(handoff.parent_session_id, owner.id);
        assert!(
            repository
                .list_pending_owner_resume_requests(owner.id)
                .expect("failed parent owner resumes")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn child_handoff_during_explicit_owner_turn_does_not_reproject_claimed_resume() {
        let (store, root_session_id) = test_repo().await;
        let (owner, existing_requests) =
            nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let child = create_sibling_session(&store, root_session_id, "racing_child").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                owner.id,
                child.id,
                "/root/owner/racing_child",
                "racing_child",
            )
            .await
            .expect("racing child edge");
        let explicit = repository
            .append_inter_agent_communication_with_protocol_bundle(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner",
                        "/root",
                        "explicit owner turn",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("explicit owner trigger");
        let owner_turn_id = TurnId::new();
        repository
            .admit_agent_triggered_turn(owner.id, owner_turn_id, explicit.history_item_id)
            .await
            .expect("explicit owner admission")
            .expect("explicit owner admitted");
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, None),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("child completion during explicit owner"),
            AdmittedTerminalCommit::Applied
        );

        let _handoff = repository
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("racing child handoff")
            .expect("turn-scoped child FINAL");
        assert_eq!(
            repository
                .schedulable_owner_resume_request_id(owner.id)
                .expect("claimed request projection"),
            None
        );
        let repository_handle = store.session_repo();
        let connection = repository_handle.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT state
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                    params![
                        owner.id.to_string(),
                        existing_requests[0].request_id.to_string()
                    ],
                    |row| row.get::<_, String>(0),
                )
                .expect("claimed old request"),
            "claimed"
        );
    }

    #[tokio::test]
    async fn explicit_agent_trigger_admission_coalesces_pending_owner_resume_rows() {
        let (store, root_session_id) = test_repo().await;
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let explicit = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/owner",
                        "/root",
                        "explicit owner work",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("explicit owner trigger");
        assert_eq!(
            store
                .session_repo()
                .schedulable_owner_resume_request_id(owner.id)
                .expect("explicit precedence projection"),
            None
        );
        let turn_id = TurnId::new();
        let admission = store
            .session_repo()
            .admit_agent_triggered_turn(owner.id, turn_id, explicit.history_item_id)
            .await
            .expect("explicit admission")
            .expect("explicit admitted");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    admission.admission_id,
                    turn_id,
                    128,
                )
                .expect("safe explicit-turn mailbox delivery")
                .history_item_ids,
            vec![requests[0].request_id.0, explicit.history_item_id],
            "the explicit admission coalesces the OwnerResume row, while its safe boundary samples both queued inputs in enqueue order"
        );
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT state || ':' || claimed_turn_id
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                    params![owner.id.to_string(), requests[0].request_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .expect("coalesced explicit state"),
            format!("claimed:{turn_id}")
        );
        drop(connection);
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    admission.admission_id,
                    &completed_terminal_for_response(owner.id, None),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("explicit terminal"),
            AdmittedTerminalCommit::Applied
        );
    }

    #[tokio::test]
    async fn two_connections_cannot_claim_one_owner_resume_request_twice() {
        let (store, root_session_id) = test_repo().await;
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let reopened_sqlite = SqliteStore::open(store.paths()).expect("second sqlite connection");
        reopened_sqlite
            .migrate()
            .expect("migrate second connection");
        let competitor = StoreBundle::new(reopened_sqlite);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let request_id = requests[0].request_id;
        let (first, second) = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first_store = &store;
            let first_thread = scope.spawn(move || {
                first_barrier.wait();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("first runtime")
                    .block_on(
                        first_store
                            .session_repo()
                            .admit_owner_resume_turn(owner.id, first_turn, request_id),
                    )
            });
            let second_barrier = barrier.clone();
            let second_store = &competitor;
            let second_thread = scope.spawn(move || {
                second_barrier.wait();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("second runtime")
                    .block_on(second_store.session_repo().admit_owner_resume_turn(
                        owner.id,
                        second_turn,
                        request_id,
                    ))
            });
            (
                first_thread.join().expect("first admission"),
                second_thread.join().expect("second admission"),
            )
        });
        let first = first.expect("first result");
        let second = second.expect("second result");
        assert_ne!(first.is_some(), second.is_some());
        let winning_turn = if first.is_some() {
            first_turn
        } else {
            second_turn
        };
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT claimed_turn_id
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                    params![owner.id.to_string(), request_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .expect("winning owner-resume claim"),
            winning_turn.to_string()
        );
    }

    #[tokio::test]
    async fn crashed_owner_resume_turn_repends_once_and_suppresses_crash_final() {
        let (store, root_session_id) = test_repo().await;
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let owner_turn_id = TurnId::new();
        let owner_admission = store
            .session_repo()
            .admit_owner_resume_turn(owner.id, owner_turn_id, requests[0].request_id)
            .await
            .expect("owner-resume admission")
            .expect("owner resume admitted");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    owner.id,
                    owner_admission.admission_id,
                    owner_turn_id,
                    128,
                )
                .expect("safe OwnerResume delivery before crash")
                .history_item_ids,
            vec![requests[0].request_id.0]
        );
        let target = store
            .session_repo()
            .captured_running_terminal_target(owner.id)
            .await
            .expect("capture owner")
            .expect("running owner");

        assert!(
            store
                .session_repo()
                .recover_captured_running_session_with_protocol_event(
                    owner.id,
                    &failed_terminal(owner.id, "owner process crashed"),
                    target,
                )
                .await
                .expect("recover owner")
        );
        assert_eq!(
            store
                .session_repo()
                .schedulable_owner_resume_request_id(owner.id)
                .expect("re-pended request"),
            Some(requests[0].request_id)
        );
        assert!(
            store
                .session_repo()
                .agent_completion_handoff(owner.id, owner_turn_id)
                .expect("suppressed crash handoff query")
                .is_none()
        );
        assert!(
            !store
                .session_repo()
                .recover_captured_running_session_with_protocol_event(
                    owner.id,
                    &failed_terminal(owner.id, "duplicate recovery"),
                    target,
                )
                .await
                .expect("idempotent retry")
        );
        assert_eq!(
            store
                .protocol_event_store()
                .list_runtime_events(owner.id, owner_turn_id)
                .expect("owner recovery events")
                .into_iter()
                .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
                .count(),
            1
        );
        assert_eq!(
            store
                .session_repo()
                .pending_deferred_completion(owner.id)
                .expect("pending crash deferred")
                .expect("recoverable crash receipt")
                .state,
            DeferredAgentCompletionState::Pending
        );
        assert!(matches!(
            store
                .session_repo()
                .settle_pending_owner_resume_with_terminal(
                    owner.id,
                    requests[0].request_id,
                    pre_admission_interrupted_terminal(),
                )
                .expect("stop pending recovered OwnerResume"),
            PendingAgentTriggerSettlement::Applied { handoff: None, .. }
        ));
        assert_eq!(
            store
                .session_repo()
                .agent_terminal_effects(owner.id, owner_turn_id)
                .expect("discarded crash effects")
                .deferred
                .expect("discarded crash receipt")
                .state,
            DeferredAgentCompletionState::Discarded
        );
        assert!(
            store
                .session_repo()
                .schedulable_owner_resume_request_id(owner.id)
                .expect("stopped OwnerResume")
                .is_none()
        );
    }

    #[tokio::test]
    async fn agent_spawn_commits_edge_context_boundary_and_initial_task_atomically() {
        let (store, root_session_id) = test_repo().await;
        let child_session_id = SessionId::new();
        let child_draft = sibling_session_draft(&store, root_session_id, "child").await;
        let rolled_back_child_session_id = SessionId::new();
        let rolled_back_child_draft =
            sibling_session_draft(&store, root_session_id, "rolled_back").await;
        let root_turn_id = TurnId::new();
        let admission = store
            .session_repo()
            .admit_session_turn(root_session_id, root_turn_id)
            .await
            .expect("root admission")
            .expect("root admitted");

        let stored = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                child_session_id,
                child_draft,
                "/root/child",
                "child",
                admission.admission_id,
                root_turn_id,
                SpawnContextFork::None,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: "Message Type: NEW_TASK\nTask name: /root/child\nSender: /root\nPayload:\nDo the bounded task.".to_string(),
                    trigger_turn: true,
                },
            )
            .expect("atomic child spawn");
        assert_eq!(stored.child_session.id, child_session_id);
        assert_eq!(stored.child_session.title, "child");
        assert_eq!(stored.edge.spawn_order, 1);
        let child_history = store
            .protocol_event_store()
            .list_history_items_for_session(child_session_id)
            .expect("child history");
        assert!(child_history.is_empty());
        let queued_initial_task = store
            .session_repo()
            .agent_mailbox_communications_by_id(
                child_session_id,
                &[stored.initial_task_history_item_id],
            )
            .expect("queued initial task");
        assert!(matches!(
            queued_initial_task.as_slice(),
            [(id, communication)]
                if *id == stored.initial_task_history_item_id
                    && communication.trigger_turn
                    && communication.author == "/root"
                    && communication.recipient == "/root/child"
        ));
        let pending_before_admission = store
            .protocol_event_store()
            .retained_descendant_page(root_session_id, 0, 10)
            .expect("retained child projection");
        assert_eq!(
            pending_before_admission.items[0].pending_trigger_history_item_id,
            Some(stored.initial_task_history_item_id)
        );
        let child_turn_id = TurnId::new();
        let child_admission = store
            .session_repo()
            .admit_agent_triggered_turn(
                child_session_id,
                child_turn_id,
                stored.initial_task_history_item_id,
            )
            .await
            .expect("child trigger admission")
            .expect("child trigger admitted");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    child_session_id,
                    child_admission.admission_id,
                    child_turn_id,
                    128,
                )
                .expect("safe initial-task delivery")
                .history_item_ids,
            vec![stored.initial_task_history_item_id]
        );
        let claimed_after_admission = store
            .protocol_event_store()
            .retained_descendant_page(root_session_id, 0, 10)
            .expect("claimed child projection");
        assert_eq!(
            claimed_after_admission.items[0].pending_trigger_history_item_id, None,
            "the atomic SessionStarted append must durably claim earlier session mail"
        );
        let delivered_initial_task = store
            .protocol_event_store()
            .history_items_by_id(child_session_id, &[stored.initial_task_history_item_id])
            .expect("delivered initial task");
        assert!(matches!(
            delivered_initial_task.as_slice(),
            [HistoryItem {
                scope: HistoryScope::Turn { turn_id },
                payload: HistoryItemPayload::InterAgentCommunication { communication },
                ..
            }] if *turn_id == child_turn_id
                && communication.trigger_turn
                && communication.author == "/root"
                && communication.recipient == "/root/child"
        ));

        let error = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                rolled_back_child_session_id,
                rolled_back_child_draft,
                "/root/rolled_back",
                "rolled_back",
                admission.admission_id,
                root_turn_id,
                SpawnContextFork::Recent(0),
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/rolled_back".to_string(),
                    content: "must roll back".to_string(),
                    trigger_turn: true,
                },
            )
            .expect_err("invalid fork must roll back the whole durable spawn");
        assert!(error.to_string().contains("requires at least one turn"));
        let edges = store
            .session_repo()
            .list_session_spawn_edges(root_session_id)
            .await
            .expect("retained edges");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].child_session_id, child_session_id);
        assert!(
            store
                .session_repo()
                .get_session(rolled_back_child_session_id)
                .await
                .is_err(),
            "context fork failure must roll back the child session row"
        );
    }

    async fn assert_spawn_rolled_back(
        store: &StoreBundle,
        root_session_id: SessionId,
        child_session_id: SessionId,
    ) {
        assert!(
            store
                .session_repo()
                .get_session(child_session_id)
                .await
                .is_err(),
            "failed spawn must not retain a child session row"
        );
        assert!(
            store
                .session_repo()
                .list_session_spawn_edges(root_session_id)
                .await
                .expect("spawn edges")
                .iter()
                .all(|edge| edge.child_session_id != child_session_id),
            "failed spawn must not retain an edge"
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(child_session_id)
                .expect("child history")
                .is_empty(),
            "failed spawn must not retain child history"
        );
    }

    #[tokio::test]
    async fn child_session_edge_fork_and_initial_task_failures_roll_back_one_spawn_transaction() {
        let (store, root_session_id) = test_repo().await;
        let root_turn_id = TurnId::new();
        let admission = store
            .session_repo()
            .admit_session_turn(root_session_id, root_turn_id)
            .await
            .expect("root admission")
            .expect("root admitted");

        let cases = [
            (
                "session_insert",
                "CREATE TEMP TRIGGER injected_agent_session_failure
                 BEFORE INSERT ON sessions
                 BEGIN
                     SELECT RAISE(ABORT, 'injected child session failure');
                 END;",
                "DROP TRIGGER injected_agent_session_failure;",
                SpawnContextFork::None,
            ),
            (
                "edge_insert",
                "CREATE TEMP TRIGGER injected_agent_edge_failure
                 BEFORE INSERT ON session_spawn_edges
                 BEGIN
                     SELECT RAISE(ABORT, 'injected child edge failure');
                 END;",
                "DROP TRIGGER injected_agent_edge_failure;",
                SpawnContextFork::None,
            ),
            (
                "initial_task",
                "CREATE TEMP TRIGGER injected_agent_task_failure
                 BEFORE INSERT ON agent_mailbox_messages
                 BEGIN
                     SELECT RAISE(ABORT, 'injected initial task failure');
                 END;",
                "DROP TRIGGER injected_agent_task_failure;",
                SpawnContextFork::None,
            ),
        ];
        for (task_name, install_trigger, drop_trigger, context_fork) in cases {
            let child_session_id = SessionId::new();
            let child_draft = sibling_session_draft(&store, root_session_id, task_name).await;
            let child_path = format!("/root/{task_name}");
            {
                let repository = store.session_repo();
                repository
                    .connection
                    .lock()
                    .expect("sqlite mutex")
                    .execute_batch(install_trigger)
                    .expect("install injected failure");
            }
            let error = store
                .session_repo()
                .create_agent_spawn_with_initial_task_for_caller_turn(
                    root_session_id,
                    root_session_id,
                    child_session_id,
                    child_draft,
                    &child_path,
                    task_name,
                    admission.admission_id,
                    root_turn_id,
                    context_fork,
                    InterAgentCommunication {
                        author: "/root".to_string(),
                        recipient: child_path.clone(),
                        content: format!("run {task_name}"),
                        trigger_turn: true,
                    },
                )
                .expect_err("injected storage failure must abort the spawn");
            assert!(error.to_string().contains("injected"));
            {
                let repository = store.session_repo();
                repository
                    .connection
                    .lock()
                    .expect("sqlite mutex")
                    .execute_batch(drop_trigger)
                    .expect("remove injected failure");
            }
            assert_spawn_rolled_back(&store, root_session_id, child_session_id).await;
        }

        let fork_child_session_id = SessionId::new();
        let fork_child_draft = sibling_session_draft(&store, root_session_id, "fork_failure").await;
        let error = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                fork_child_session_id,
                fork_child_draft,
                "/root/fork_failure",
                "fork_failure",
                admission.admission_id,
                root_turn_id,
                SpawnContextFork::Recent(0),
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/fork_failure".to_string(),
                    content: "invalid recent fork".to_string(),
                    trigger_turn: true,
                },
            )
            .expect_err("invalid context fork must abort the spawn");
        assert!(error.to_string().contains("requires at least one turn"));
        assert_spawn_rolled_back(&store, root_session_id, fork_child_session_id).await;

        let stale_child_session_id = SessionId::new();
        let stale_child_draft = sibling_session_draft(&store, root_session_id, "stale_owner").await;
        let error = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                stale_child_session_id,
                stale_child_draft,
                "/root/stale_owner",
                "stale_owner",
                AdmissionId::new(),
                root_turn_id,
                SpawnContextFork::None,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/stale_owner".to_string(),
                    content: "stale caller".to_string(),
                    trigger_turn: true,
                },
            )
            .expect_err("stale caller admission must abort before child creation");
        assert!(error.to_string().contains("no longer owns active turn"));
        assert_spawn_rolled_back(&store, root_session_id, stale_child_session_id).await;
    }

    #[tokio::test]
    async fn nested_agent_spawn_binds_initial_task_to_the_immediate_parent_path() {
        let (store, root_session_id) = test_repo().await;
        let root_turn_id = TurnId::new();
        let root_admission = store
            .session_repo()
            .admit_session_turn(root_session_id, root_turn_id)
            .await
            .expect("root admission")
            .expect("root admitted");
        let parent_session_id = SessionId::new();
        let parent_draft = sibling_session_draft(&store, root_session_id, "parent").await;
        store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                parent_session_id,
                parent_draft,
                "/root/parent",
                "parent",
                root_admission.admission_id,
                root_turn_id,
                SpawnContextFork::None,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/parent".to_string(),
                    content: "parent task".to_string(),
                    trigger_turn: true,
                },
            )
            .expect("parent spawn");
        let parent_turn_id = TurnId::new();
        let parent_admission = store
            .session_repo()
            .admit_session_turn(parent_session_id, parent_turn_id)
            .await
            .expect("parent admission")
            .expect("parent admitted");

        let child_session_id = SessionId::new();
        let child_draft = sibling_session_draft(&store, root_session_id, "child").await;
        let stored = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                parent_session_id,
                child_session_id,
                child_draft,
                "/root/parent/child",
                "child",
                parent_admission.admission_id,
                parent_turn_id,
                SpawnContextFork::None,
                InterAgentCommunication {
                    author: "/root/parent".to_string(),
                    recipient: "/root/parent/child".to_string(),
                    content: "nested task".to_string(),
                    trigger_turn: true,
                },
            )
            .expect("nested spawn");
        assert_eq!(stored.edge.parent_session_id, parent_session_id);
        assert_eq!(stored.edge.agent_path, "/root/parent/child");
        let child_history = store
            .protocol_event_store()
            .list_history_items_for_session(child_session_id)
            .expect("nested child history");
        assert!(child_history.is_empty());
        let queued_initial_task = store
            .session_repo()
            .agent_mailbox_communications_by_id(
                child_session_id,
                &[stored.initial_task_history_item_id],
            )
            .expect("nested queued initial task");
        assert!(matches!(
            queued_initial_task.as_slice(),
            [(id, communication)] if *id == stored.initial_task_history_item_id
                && communication.author == "/root/parent"
                && communication.recipient == "/root/parent/child"
                && communication.trigger_turn
        ));
        let child_turn_id = TurnId::new();
        let child_admission = store
            .session_repo()
            .admit_agent_triggered_turn(
                child_session_id,
                child_turn_id,
                stored.initial_task_history_item_id,
            )
            .await
            .expect("nested child trigger admission")
            .expect("nested child admitted");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    child_session_id,
                    child_admission.admission_id,
                    child_turn_id,
                    128,
                )
                .expect("nested safe initial-task delivery")
                .history_item_ids,
            vec![stored.initial_task_history_item_id]
        );

        let wrong_author_session_id = SessionId::new();
        let wrong_author_draft =
            sibling_session_draft(&store, root_session_id, "wrong_author").await;
        let error = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                parent_session_id,
                wrong_author_session_id,
                wrong_author_draft,
                "/root/parent/wrong_author",
                "wrong_author",
                parent_admission.admission_id,
                parent_turn_id,
                SpawnContextFork::None,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/parent/wrong_author".to_string(),
                    content: "wrong sender".to_string(),
                    trigger_turn: true,
                },
            )
            .expect_err("nested initial task must use its immediate parent author");
        assert!(error.to_string().contains("immediate parent"));
        assert_spawn_rolled_back(&store, root_session_id, wrong_author_session_id).await;
    }

    #[tokio::test]
    async fn spawn_edge_repository_accepts_recursive_lineage_and_rejects_invalid_parentage() {
        let (store, root_session_id) = test_repo().await;
        let direct = create_sibling_session(&store, root_session_id, "direct").await;
        let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
        let wrong_path = create_sibling_session(&store, root_session_id, "wrong_path").await;
        let unattached_parent =
            create_sibling_session(&store, root_session_id, "unattached_parent").await;
        let unattached_child =
            create_sibling_session(&store, root_session_id, "unattached_child").await;
        let other_root = create_sibling_session(&store, root_session_id, "other_root").await;
        let other_parent = create_sibling_session(&store, root_session_id, "other_parent").await;
        let cross_tree_child =
            create_sibling_session(&store, root_session_id, "cross_tree_child").await;
        let invalid_name = create_sibling_session(&store, root_session_id, "invalid_name").await;
        let repository = store.session_repo();

        let direct_edge = repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                direct.id,
                "/root/direct",
                "direct",
            )
            .await
            .expect("direct edge");
        let grandchild_edge = repository
            .insert_session_spawn_edge(
                root_session_id,
                direct.id,
                grandchild.id,
                "/root/direct/grandchild",
                "grandchild",
            )
            .await
            .expect("nested edge");
        repository
            .insert_session_spawn_edge(
                other_root.id,
                other_root.id,
                other_parent.id,
                "/root/other_parent",
                "other_parent",
            )
            .await
            .expect("other tree parent");

        assert_eq!(direct_edge.parent_session_id, root_session_id);
        assert_eq!(direct_edge.agent_path, "/root/direct");
        assert_eq!(grandchild_edge.parent_session_id, direct.id);
        assert_eq!(grandchild_edge.agent_path, "/root/direct/grandchild");
        assert_eq!(
            repository
                .session_spawn_edge_for_child(grandchild.id)
                .await
                .expect("grandchild lookup"),
            Some(grandchild_edge.clone())
        );

        let cases = [
            (
                direct.id,
                wrong_path.id,
                "/root/wrong_path",
                "wrong_path",
                "does not match canonical parent/task path",
            ),
            (
                unattached_parent.id,
                unattached_child.id,
                "/root/unattached_parent/unattached_child",
                "unattached_child",
                "is not a retained agent in root tree",
            ),
            (
                other_parent.id,
                cross_tree_child.id,
                "/root/other_parent/cross_tree_child",
                "cross_tree_child",
                "is not a retained agent in root tree",
            ),
            (
                root_session_id,
                invalid_name.id,
                "/root/BadName",
                "BadName",
                "invalid task name",
            ),
            (
                root_session_id,
                root_session_id,
                "/root/self",
                "self",
                "cannot also be its own child",
            ),
        ];
        for (parent_session_id, child_session_id, path, task_name, expected) in cases {
            let error = repository
                .insert_session_spawn_edge(
                    root_session_id,
                    parent_session_id,
                    child_session_id,
                    path,
                    task_name,
                )
                .await
                .expect_err("invalid edge must fail before SQLite mutation");
            assert!(
                error.to_string().contains(expected),
                "unexpected error: {error}"
            );
        }

        assert_eq!(
            repository
                .list_session_spawn_edges(root_session_id)
                .await
                .expect("recursive edges"),
            vec![direct_edge.clone(), grandchild_edge.clone()]
        );
        assert_eq!(
            repository
                .list_session_subtree_ids(root_session_id)
                .await
                .expect("root subtree"),
            vec![root_session_id, direct.id, grandchild.id]
        );
        assert_eq!(
            repository
                .list_session_subtree_ids(direct.id)
                .await
                .expect("nested subtree"),
            vec![direct.id, grandchild.id]
        );
        let initial_states = repository
            .list_descendant_run_admission_states(root_session_id)
            .await
            .expect("initial descendant states");
        assert_eq!(initial_states.len(), 2);
        assert!(
            initial_states
                .iter()
                .all(|state| !state.blocks_new_root_turn)
        );
        repository
            .admit_session_turn(grandchild.id, TurnId::new())
            .await
            .expect("grandchild admission")
            .expect("grandchild admitted");
        let admitted_states = repository
            .list_descendant_run_admission_states(root_session_id)
            .await
            .expect("admitted descendant states");
        assert_eq!(admitted_states.len(), 2);
        assert!(
            admitted_states.iter().any(|state| {
                state.edge.child_session_id == grandchild.id && state.blocks_new_root_turn
            }),
            "a running grandchild must block a new root turn"
        );
        assert!(
            admitted_states.iter().any(|state| {
                state.edge.child_session_id == direct.id && !state.blocks_new_root_turn
            }),
            "an idle direct parent must remain non-blocking"
        );
        assert!(
            repository.get_session(cross_tree_child.id).await.is_ok(),
            "a rejected cross-tree edge must not delete the independent session"
        );
    }

    #[tokio::test]
    async fn spawn_edge_repository_enforces_one_project_and_one_tree_owner_per_session() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        let nested_candidate =
            create_sibling_session(&store, root_session_id, "nested_candidate").await;
        let root_child_candidate =
            create_sibling_session(&store, root_session_id, "root_child_candidate").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("owned child");

        let child_as_root = repository
            .insert_session_spawn_edge(
                child.id,
                child.id,
                nested_candidate.id,
                "/root/nested_candidate",
                "nested_candidate",
            )
            .await
            .expect_err("a retained child cannot own a second tree");
        assert!(child_as_root.to_string().contains("retained descendant"));

        let existing_root_as_child = repository
            .insert_session_spawn_edge(
                root_child_candidate.id,
                root_child_candidate.id,
                root_session_id,
                "/root/root",
                "root",
            )
            .await
            .expect_err("an existing tree root cannot become a child");
        assert!(
            existing_root_as_child
                .to_string()
                .contains("invalid task name")
                || existing_root_as_child
                    .to_string()
                    .contains("already owns an agent tree")
        );

        let root = repository
            .get_session(root_session_id)
            .await
            .expect("root session");
        let foreign_project_id = ProjectId::new();
        let foreign_root = root.cwd.join("foreign-project");
        store
            .project_repo()
            .upsert_project(foreign_project_id, &foreign_root, "foreign project", "none")
            .await
            .expect("foreign project");
        let foreign_child = repository
            .create_session(NewSession {
                project_id: foreign_project_id,
                title: "foreign child".to_string(),
                cwd: foreign_root,
                model: root.model,
                base_url: root.base_url,
                access_mode: root.access_mode,
            })
            .await
            .expect("foreign child");
        let cross_project = repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                foreign_child.id,
                "/root/foreign_child",
                "foreign_child",
            )
            .await
            .expect_err("cross-project child must be rejected");
        assert!(cross_project.to_string().contains("belong to one project"));
    }

    #[tokio::test]
    async fn deleting_a_nested_subtree_preserves_its_ancestor_and_sibling() {
        let (store, root_session_id) = test_repo().await;
        let parent = create_sibling_session(&store, root_session_id, "parent").await;
        let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
        let sibling = create_sibling_session(&store, root_session_id, "sibling").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                parent.id,
                "/root/parent",
                "parent",
            )
            .await
            .expect("parent edge");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                parent.id,
                grandchild.id,
                "/root/parent/grandchild",
                "grandchild",
            )
            .await
            .expect("grandchild edge");
        let sibling_edge = repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                sibling.id,
                "/root/sibling",
                "sibling",
            )
            .await
            .expect("sibling edge");

        let deleted = repository
            .delete_session_tree(parent.id)
            .await
            .expect("nested subtree delete");

        assert_eq!(deleted, vec![grandchild.id, parent.id]);
        assert!(repository.get_session(root_session_id).await.is_ok());
        assert!(repository.get_session(sibling.id).await.is_ok());
        assert!(repository.get_session(parent.id).await.is_err());
        assert!(repository.get_session(grandchild.id).await.is_err());
        assert_eq!(
            repository
                .list_session_spawn_edges(root_session_id)
                .await
                .expect("remaining tree"),
            vec![sibling_edge]
        );
    }

    #[tokio::test]
    async fn deleting_a_root_tree_removes_delivered_recipient_mail_before_history() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let stored = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: "delivered before root-tree deletion".to_string(),
                    trigger_turn: false,
                },
                true,
            )
            .expect("pending child mail");
        let delivered = store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                child.id,
                child_admission_id,
                child_turn_id,
                128,
            )
            .expect("deliver child mail");
        assert_eq!(delivered.history_item_ids, vec![stored.history_item_id]);
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &agent_interrupted_terminal(child.id),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("close child turn"),
            AdmittedTerminalCommit::Applied
        );

        let deleted = store
            .session_repo()
            .delete_session_tree(root_session_id)
            .await
            .expect("delete complete root tree");
        assert_eq!(deleted, vec![child.id, root_session_id]);
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM agent_mailbox_messages", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("mailbox count"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM protocol_history_items", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("history count"),
            0
        );
    }

    #[tokio::test]
    async fn deleting_a_nested_author_retains_delivered_owner_mail_with_tombstoned_author() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let stored = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                root_session_id,
                InterAgentCommunication {
                    author: "/root/child".to_string(),
                    recipient: "/root".to_string(),
                    content: "completed evidence retained by owner".to_string(),
                    trigger_turn: false,
                },
                false,
            )
            .expect("pending owner mail");
        let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
        let delivered = store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                root_session_id,
                root_admission_id,
                root_turn_id,
                128,
            )
            .expect("deliver owner mail");
        assert_eq!(delivered.history_item_ids, vec![stored.history_item_id]);
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    root_admission_id,
                    &agent_interrupted_terminal(root_session_id),
                    root_turn_id,
                    None,
                    None,
                )
                .await
                .expect("close owner turn"),
            AdmittedTerminalCommit::Applied
        );

        assert_eq!(
            store
                .session_repo()
                .delete_session_tree(child.id)
                .await
                .expect("delete completed child"),
            vec![child.id]
        );
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        let retained = connection
            .query_row(
                "SELECT author_session_id, recipient_session_id, state
                 FROM agent_mailbox_messages
                 WHERE id = ?1",
                params![stored.history_item_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("retained owner mailbox");
        assert_eq!(retained.0, None);
        assert_eq!(retained.1, root_session_id.to_string());
        assert_eq!(retained.2, "delivered");
        assert!(
            connection
                .query_row(
                    "SELECT EXISTS(
                     SELECT 1
                     FROM protocol_history_items
                     WHERE id = ?1 AND session_id = ?2
                 )",
                    params![
                        stored.history_item_id.to_string(),
                        root_session_id.to_string()
                    ],
                    |row| row.get::<_, bool>(0),
                )
                .expect("retained owner history")
        );
    }

    #[tokio::test]
    async fn deleting_a_nested_author_rejects_pending_mail_owned_by_ancestor() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let stored = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                root_session_id,
                InterAgentCommunication {
                    author: "/root/child".to_string(),
                    recipient: "/root".to_string(),
                    content: "owner has not observed this yet".to_string(),
                    trigger_turn: false,
                },
                false,
            )
            .expect("pending owner mail");

        let error = store
            .session_repo()
            .delete_session_tree(child.id)
            .await
            .expect_err("pending outgoing owner mail must block author deletion");
        assert!(error.to_string().contains("pending owner mail exists"));
        assert!(
            error
                .to_string()
                .contains(&stored.history_item_id.to_string())
        );
        assert!(store.session_repo().get_session(child.id).await.is_ok());
        assert_eq!(
            store
                .session_repo()
                .agent_mailbox_communications_by_id(root_session_id, &[stored.history_item_id],)
                .expect("pending owner mailbox")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn descendant_tree_mutation_is_blocked_by_its_active_ancestor_owner() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        repository
            .admit_session_turn(root_session_id, TurnId::new())
            .await
            .expect("root admission")
            .expect("root admitted");

        assert_eq!(
            repository
                .mutation_blocker_in_session_tree(child.id)
                .await
                .expect("descendant mutation blocker"),
            Some(root_session_id)
        );
        let archive_error = repository
            .set_session_archived(child.id, true)
            .await
            .expect_err("active ancestor must block descendant archive");
        assert!(
            archive_error
                .to_string()
                .contains(&root_session_id.to_string())
        );
        let delete_error = repository
            .delete_session_tree(child.id)
            .await
            .expect_err("active ancestor must block descendant delete");
        assert!(
            delete_error
                .to_string()
                .contains(&root_session_id.to_string())
        );
        assert!(repository.get_session(child.id).await.is_ok());
    }

    async fn assert_pending_trigger_blocks_destructive_mutations(
        store: &StoreBundle,
        root_session_id: SessionId,
        child_session_id: SessionId,
        project_id: ProjectId,
    ) {
        let repository = store.session_repo();
        assert_eq!(
            repository
                .mutation_blocker_in_session_tree(root_session_id)
                .await
                .expect("root mutation blocker"),
            Some(child_session_id)
        );
        assert_eq!(
            repository
                .mutation_blocker_in_session_tree(child_session_id)
                .await
                .expect("descendant mutation blocker"),
            Some(child_session_id)
        );
        let rollback_error = repository
            .rollback_session_transaction(root_session_id, 1)
            .await
            .expect_err("pending trigger must block root rollback");
        assert!(
            rollback_error
                .to_string()
                .contains(&child_session_id.to_string())
        );
        let archive_error = repository
            .set_session_archived(child_session_id, true)
            .await
            .expect_err("pending trigger must block descendant archive");
        assert!(
            archive_error
                .to_string()
                .contains(&child_session_id.to_string())
        );
        let delete_error = repository
            .delete_session_tree(child_session_id)
            .await
            .expect_err("pending trigger must block descendant delete");
        assert!(
            delete_error
                .to_string()
                .contains(&child_session_id.to_string())
        );
        let project_error = store
            .project_repo()
            .delete_project(project_id)
            .await
            .expect_err("pending trigger must block project delete");
        assert!(
            project_error
                .to_string()
                .contains(&child_session_id.to_string())
        );
        assert!(repository.get_session(root_session_id).await.is_ok());
        assert!(repository.get_session(child_session_id).await.is_ok());
    }

    #[tokio::test]
    async fn pending_trigger_blocks_destructive_mutation_before_and_after_store_restart() {
        let (store, root_session_id) = test_repo().await;
        let root = store
            .session_repo()
            .get_session(root_session_id)
            .await
            .expect("root session");
        let child = create_sibling_session(&store, root_session_id, "pending").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/pending",
                "pending",
            )
            .await
            .expect("pending child edge");
        let stored = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/pending".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/pending",
                        "/root",
                        "run after restart",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("pending child trigger");
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger"),
            Some(stored.history_item_id)
        );

        assert_pending_trigger_blocks_destructive_mutations(
            &store,
            root_session_id,
            child.id,
            root.project_id,
        )
        .await;

        let paths = store.paths().clone();
        drop(store);
        let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
        reopened_sqlite.migrate().expect("migrate reopened store");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert_pending_trigger_blocks_destructive_mutations(
            &reopened,
            root_session_id,
            child.id,
            root.project_id,
        )
        .await;
    }

    #[tokio::test]
    async fn repository_mutations_fail_closed_for_durable_active_tree_state() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "active_child").await;
        let repository = store.session_repo();
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/active_child",
                "active_child",
            )
            .await
            .expect("child edge");
        repository
            .admit_session_turn(child.id, TurnId::new())
            .await
            .expect("child admission")
            .expect("child admitted");

        let archive_error = repository
            .set_session_archived(root_session_id, true)
            .await
            .expect_err("active child must block repository-level root archive");
        assert!(archive_error.to_string().contains(&child.id.to_string()));
        let delete_error = repository
            .delete_session_tree(root_session_id)
            .await
            .expect_err("active child must block repository-level tree delete");
        assert!(delete_error.to_string().contains(&child.id.to_string()));
        let settings_error = repository
            .update_session_settings(
                child.id,
                &SessionSettingsPatch {
                    model: Some("changed-model".to_string()),
                    ..SessionSettingsPatch::default()
                },
            )
            .await
            .expect_err("active target must block repository-level settings mutation");
        assert!(settings_error.to_string().contains("active"));
        assert!(repository.get_session(root_session_id).await.is_ok());
        assert!(repository.get_session(child.id).await.is_ok());
    }

    #[tokio::test]
    async fn runtime_state_contract_rejects_partial_and_impossible_owners_across_readers() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let project_id = repository
            .get_session(session_id)
            .await
            .expect("initial session")
            .project_id;
        let admission_id = AdmissionId::new();
        let turn_id = TurnId::new();
        let lease_expires_at_ms = normalize_run_lease_now_ms(SystemClock::now_ms()) + 60_000;
        let admission_id_text = admission_id.to_string();
        let turn_id_text = turn_id.to_string();
        let cases = [
            ("running without owner", "running", None, None, None),
            (
                "running partial owner",
                "running",
                Some(admission_id_text.as_str()),
                None,
                Some(lease_expires_at_ms),
            ),
            (
                "terminal partial owner",
                "completed",
                None,
                Some(turn_id_text.as_str()),
                Some(lease_expires_at_ms),
            ),
            (
                "idle retained owner",
                "idle",
                Some(admission_id_text.as_str()),
                Some(turn_id_text.as_str()),
                Some(lease_expires_at_ms),
            ),
            (
                "invalid admission identity",
                "running",
                Some("not-an-admission"),
                Some(turn_id_text.as_str()),
                Some(lease_expires_at_ms),
            ),
            (
                "invalid turn identity",
                "running",
                Some(admission_id_text.as_str()),
                Some("not-a-turn"),
                Some(lease_expires_at_ms),
            ),
            (
                "nonpositive lease",
                "running",
                Some(admission_id_text.as_str()),
                Some(turn_id_text.as_str()),
                Some(0),
            ),
        ];

        for (label, status, active_run_id, active_turn_id, lease_expires_at_ms) in cases {
            {
                let connection = repository.connection.lock().expect("sqlite mutex poisoned");
                connection
                    .execute(
                        "UPDATE sessions
                         SET status = ?2,
                             active_run_id = ?3,
                             active_turn_id = ?4,
                             active_run_lease_expires_at_ms = ?5
                         WHERE id = ?1",
                        params![
                            session_id.to_string(),
                            status,
                            active_run_id,
                            active_turn_id,
                            lease_expires_at_ms,
                        ],
                    )
                    .expect("inject invalid runtime state");
            }

            assert!(repository.get_session(session_id).await.is_err(), "{label}");
            assert!(
                repository.latest_session(project_id).await.is_err(),
                "{label}"
            );
            assert!(
                repository
                    .session_projection_state(session_id)
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository.list_sessions(project_id, 10).await.is_err(),
                "{label}"
            );
            assert!(
                repository
                    .has_fresh_run_admission(session_id)
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository
                    .fresh_running_turn_for_session(session_id)
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository
                    .session_blocks_mutation(session_id)
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository
                    .mutation_blocker_in_session_tree(session_id)
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository
                    .active_session_for_project(project_id)
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository
                    .admitted_run_status_at(
                        session_id,
                        admission_id,
                        turn_id,
                        SystemClock::now_ms(),
                    )
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository
                    .renew_admitted_run_lease_at(
                        session_id,
                        admission_id,
                        turn_id,
                        SystemClock::now_ms(),
                        RUN_ADMISSION_LEASE_DURATION_MS,
                    )
                    .await
                    .is_err(),
                "{label}"
            );
            assert!(
                repository
                    .release_stopped_run_admission(session_id, admission_id)
                    .await
                    .is_err(),
                "{label}"
            );
        }
    }

    #[tokio::test]
    async fn project_and_tree_gates_validate_later_corrupt_rows_after_a_valid_blocker() {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let project_id = repository
            .get_session(root_session_id)
            .await
            .expect("root session")
            .project_id;
        let child = create_sibling_session(&store, root_session_id, "corrupt_child").await;
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/corrupt_child",
                "corrupt_child",
            )
            .await
            .expect("child edge");
        let corrupt_admission_id = AdmissionId::new().to_string();
        repository
            .inject_raw_runtime_state_for_corruption_test(
                child.id,
                "completed",
                Some(&corrupt_admission_id),
                None,
                None,
            )
            .expect("inject later corrupt child");
        repository
            .admit_session_turn(root_session_id, TurnId::new())
            .await
            .expect("root admission")
            .expect("root admitted");
        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            connection
                .execute(
                    "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
                    params![root_session_id.to_string(), i64::MAX - 1],
                )
                .expect("order valid blocker before corrupt child");
        }

        let tree_error = repository
            .mutation_blocker_in_session_tree(root_session_id)
            .await
            .expect_err("tree gate must validate the later corrupt child");
        assert!(
            tree_error
                .to_string()
                .contains("incomplete durable run admission")
        );
        let project_error = repository
            .active_session_for_project(project_id)
            .await
            .expect_err("project gate must validate the later corrupt child");
        assert!(
            project_error
                .to_string()
                .contains("incomplete durable run admission")
        );
    }

    #[tokio::test]
    async fn project_gate_includes_unknown_status_rows_without_owner_columns() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let project_id = repository
            .get_session(session_id)
            .await
            .expect("session")
            .project_id;
        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            connection
                .execute_batch("PRAGMA ignore_check_constraints = ON")
                .expect("enable corruption fixture");
            connection
                .execute(
                    "UPDATE sessions SET status = 'unknown_status' WHERE id = ?1",
                    params![session_id.to_string()],
                )
                .expect("inject unknown status");
            connection
                .execute_batch("PRAGMA ignore_check_constraints = OFF")
                .expect("restore constraints");
        }

        let error = repository
            .active_session_for_project(project_id)
            .await
            .expect_err("unknown status must be decoded and rejected");
        assert!(
            error
                .to_string()
                .contains("unknown persisted session status")
        );
    }

    #[tokio::test]
    async fn persisted_access_mode_is_fail_closed_instead_of_defaulting() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            connection
                .execute_batch("PRAGMA ignore_check_constraints = ON")
                .expect("enable corruption fixture");
            connection
                .execute(
                    "UPDATE sessions SET access_mode = 'unknown_access' WHERE id = ?1",
                    params![session_id.to_string()],
                )
                .expect("inject unknown access mode");
            connection
                .execute_batch("PRAGMA ignore_check_constraints = OFF")
                .expect("restore constraints");
        }

        let error = repository
            .get_session(session_id)
            .await
            .expect_err("unknown persisted access mode must fail closed");
        assert!(error.to_string().contains("unknown persisted access mode"));
    }

    #[tokio::test]
    async fn persisted_auto_review_access_mode_round_trips_through_the_typed_repository() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let update = repository
            .compare_and_set_root_session_access_mode(
                session_id,
                AccessMode::Default,
                AccessMode::AutoReview,
            )
            .await
            .expect("auto-review access update")
            .expect("matching access owner");

        assert!(update.changed);
        assert_eq!(update.session.access_mode, AccessMode::AutoReview);
        assert_eq!(
            repository
                .get_session(session_id)
                .await
                .expect("persisted session")
                .access_mode,
            AccessMode::AutoReview
        );
    }

    async fn active_turn(store: &StoreBundle, session_id: SessionId) -> (AdmissionId, TurnId) {
        let repo = store.session_repo();
        let turn_id = TurnId::new();
        let admission_id = repo
            .admit_session_turn(session_id, turn_id)
            .await
            .expect("admit")
            .expect("admitted")
            .admission_id;
        repo.append_user_turn_with_protocol_bundle(
            session_id,
            admission_id,
            &UserTurn {
                turn_id,
                items: vec![UserInputItem::Text {
                    text: "canonical request".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
            turn_id,
            0,
        )
        .await
        .expect("user turn");
        (admission_id, turn_id)
    }

    #[tokio::test]
    async fn failed_steer_transaction_is_invisible_and_retry_commits_once() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let repository = store.session_repo();
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER abort_steer_queue
                 BEFORE INSERT ON turn_steer_inputs
                 WHEN NEW.origin_kind = 'runtime'
                 BEGIN SELECT RAISE(ABORT, 'injected steer queue failure'); END;",
            )
            .expect("failure trigger");
        let steer = SteerTurn {
            expected_turn_id: turn_id,
            items: vec![UserInputItem::Text {
                text: "retry this steer".to_string(),
            }],
            additional_context: Default::default(),
            client_user_message_id: Some("retry-steer".to_string()),
        };

        repository
            .accept_active_turn_steer(session_id, &steer)
            .await
            .expect_err("injected transaction failure");
        assert_eq!(
            repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .query_row(
                    "SELECT COUNT(*)
                     FROM turn_steer_inputs
                     WHERE session_id = ?1",
                    [session_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("queue count after failure"),
            0
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .expect("history after failure")
                .iter()
                .all(|item| !matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
        );
        assert!(
            store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("events after failure")
                .iter()
                .all(|event| !matches!(event.msg, RuntimeEventMsg::SteerInputAccepted { .. }))
        );
        assert!(
            store
                .protocol_event_store()
                .list_turn_items(session_id, turn_id)
                .expect("turn items after failure")
                .iter()
                .all(|item| !matches!(item.payload, TurnItemPayload::SteerMessage { .. }))
        );

        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch("DROP TRIGGER abort_steer_queue;")
            .expect("drop failure trigger");
        let committed_id = repository
            .accept_active_turn_steer(session_id, &steer)
            .await
            .expect("retry steer");
        assert!(
            repository
                .has_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
                .expect("pending retry steer")
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .expect("history before safe delivery")
                .iter()
                .all(|item| !matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
        );
        assert_eq!(
            repository
                .deliver_all_pending_turn_steers_for_admitted_turn(
                    session_id,
                    admission_id,
                    turn_id,
                )
                .expect("safe steer delivery"),
            vec![committed_id]
        );
        let committed = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history after retry")
            .into_iter()
            .filter(|item| matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
            .collect::<Vec<_>>();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].id, committed_id);
        assert!(
            !repository
                .has_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
                .expect("no pending steer after delivery")
        );
    }

    #[tokio::test]
    async fn queued_steers_deliver_in_fifo_order_with_exact_history_identities() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let repository = store.session_repo();
        let mut accepted_ids = Vec::new();
        for index in 0..3 {
            accepted_ids.push(
                repository
                    .accept_active_turn_steer(
                        session_id,
                        &SteerTurn {
                            expected_turn_id: turn_id,
                            items: vec![UserInputItem::Text {
                                text: format!("fifo-steer-{index}"),
                            }],
                            additional_context: Default::default(),
                            client_user_message_id: Some(format!("fifo-client-{index}")),
                        },
                    )
                    .await
                    .expect("queue FIFO steer"),
            );
        }
        assert!(
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .expect("history before FIFO delivery")
                .iter()
                .all(|item| !matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
        );

        assert_eq!(
            repository
                .deliver_all_pending_turn_steers_for_admitted_turn(
                    session_id,
                    admission_id,
                    turn_id,
                )
                .expect("deliver FIFO batch"),
            accepted_ids
        );
        let delivered_ids = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history after FIFO delivery")
            .into_iter()
            .filter_map(|item| {
                matches!(item.payload, HistoryItemPayload::SteerTurn { .. }).then_some(item.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(delivered_ids, accepted_ids);
        assert_eq!(
            store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events after FIFO delivery")
                .into_iter()
                .filter(|event| { matches!(event.msg, RuntimeEventMsg::SteerInputAccepted { .. }) })
                .count(),
            accepted_ids.len()
        );
    }

    #[tokio::test]
    async fn failed_delivery_rolls_back_the_whole_batch_and_retry_keeps_the_same_id() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let repository = store.session_repo();
        let input_id = repository
            .accept_active_turn_steer(
                session_id,
                &SteerTurn {
                    expected_turn_id: turn_id,
                    items: vec![UserInputItem::Text {
                        text: "atomic delivery".to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("atomic-delivery".to_string()),
                },
            )
            .await
            .expect("queue steer");
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER abort_steer_turn_projection
                 BEFORE INSERT ON protocol_turn_items
                 WHEN json_extract(NEW.payload_json, '$.kind') = 'steer_message'
                 BEGIN SELECT RAISE(ABORT, 'injected steer projection failure'); END;",
            )
            .expect("install delivery failure");

        let error = repository
            .deliver_all_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id)
            .expect_err("delivery bundle must roll back");
        assert!(
            error
                .to_string()
                .contains("injected steer projection failure")
        );
        assert!(
            repository
                .has_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
                .expect("queue retained after rollback")
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .expect("history after rollback")
                .iter()
                .all(|item| item.id != input_id)
        );
        assert!(
            store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events after rollback")
                .iter()
                .all(|event| !matches!(event.msg, RuntimeEventMsg::SteerInputAccepted { .. }))
        );

        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch("DROP TRIGGER abort_steer_turn_projection")
            .expect("remove delivery failure");
        assert_eq!(
            repository
                .deliver_all_pending_turn_steers_for_admitted_turn(
                    session_id,
                    admission_id,
                    turn_id,
                )
                .expect("retry delivery"),
            vec![input_id]
        );
    }

    #[tokio::test]
    async fn normal_terminal_finish_drains_late_steer_but_interruption_discards_it() {
        let (completed_store, completed_session_id) = test_repo().await;
        let (completed_admission_id, completed_turn_id) =
            active_turn(&completed_store, completed_session_id).await;
        let completed_repo = completed_store.session_repo();
        let completed_input_id = completed_repo
            .accept_active_turn_steer(
                completed_session_id,
                &SteerTurn {
                    expected_turn_id: completed_turn_id,
                    items: vec![UserInputItem::Text {
                        text: "late normal steer".to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("late-normal".to_string()),
                },
            )
            .await
            .expect("queue late normal steer");
        assert_eq!(
            completed_repo
                .terminalize_admitted_turn_with_protocol_event(
                    completed_session_id,
                    completed_admission_id,
                    &completed_terminal_for_response(completed_session_id, None),
                    completed_turn_id,
                    None,
                    None,
                )
                .await
                .expect("normal terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            completed_store
                .protocol_event_store()
                .list_history_items(completed_session_id, completed_turn_id)
                .expect("normal terminal history")
                .iter()
                .any(|item| item.id == completed_input_id)
        );
        let completed_state = completed_repo
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT state, delivered_history_item_id,
                        resolved_by_terminal_event_id
                 FROM turn_steer_inputs
                 WHERE id = ?1",
                [completed_input_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .expect("normal terminal queue state");
        assert_eq!(
            completed_state,
            (
                "delivered".to_string(),
                Some(completed_input_id.to_string()),
                None
            )
        );

        let (interrupted_store, interrupted_session_id) = test_repo().await;
        let (interrupted_admission_id, interrupted_turn_id) =
            active_turn(&interrupted_store, interrupted_session_id).await;
        let interrupted_repo = interrupted_store.session_repo();
        let interrupted_input_id = interrupted_repo
            .accept_active_turn_steer(
                interrupted_session_id,
                &SteerTurn {
                    expected_turn_id: interrupted_turn_id,
                    items: vec![UserInputItem::Text {
                        text: "discard on interrupt".to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("interrupt-discard".to_string()),
                },
            )
            .await
            .expect("queue interrupt steer");
        assert_eq!(
            interrupted_repo
                .terminalize_admitted_turn_with_protocol_event(
                    interrupted_session_id,
                    interrupted_admission_id,
                    &agent_interrupted_terminal(interrupted_session_id),
                    interrupted_turn_id,
                    None,
                    None,
                )
                .await
                .expect("interrupt terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            interrupted_store
                .protocol_event_store()
                .list_history_items(interrupted_session_id, interrupted_turn_id)
                .expect("interrupt history")
                .iter()
                .all(|item| item.id != interrupted_input_id)
        );
        let (state, resolver_id) = interrupted_repo
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT state, resolved_by_terminal_event_id
                 FROM turn_steer_inputs
                 WHERE id = ?1",
                [interrupted_input_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("interrupt queue state");
        assert_eq!(state, "discarded");
        let resolver_id = resolver_id
            .parse::<RuntimeEventId>()
            .expect("typed interrupt terminal id");
        let resolver = interrupted_store
            .protocol_event_store()
            .list_runtime_events(interrupted_session_id, interrupted_turn_id)
            .expect("interrupt resolver events")
            .into_iter()
            .filter(|event| event.id == resolver_id)
            .collect::<Vec<_>>();
        assert!(matches!(
            resolver.as_slice(),
            [RuntimeEvent {
                msg: RuntimeEventMsg::TurnTerminal { terminal },
                ..
            }] if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. })
        ));
    }

    async fn expire_and_recover_run(store: &StoreBundle, session_id: SessionId) -> AdmissionId {
        let recovery_now = SystemClock::now_ms()
            .saturating_add(RUN_ADMISSION_LEASE_DURATION_MS)
            .saturating_add(1_000);
        store
            .session_repo()
            .admit_session_turn_at(
                session_id,
                TurnId::new(),
                recovery_now,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect("recover expired admission")
            .expect("admit replacement run")
            .admission_id
    }

    fn completed_terminal(session_id: SessionId) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Completed,
                final_response_id: Some(ModelResponseId::new()),
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    fn completed_terminal_for_response(
        session_id: SessionId,
        final_response_id: Option<ModelResponseId>,
    ) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Completed,
                final_response_id,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    async fn record_text_response(
        store: &StoreBundle,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        text: &str,
    ) -> ModelResponseId {
        let response_id = ModelResponseId::new();
        store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id,
                    assistant_text: Some(text.to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: Vec::new(),
                },
            )
            .await
            .expect("record assistant response");
        response_id
    }

    async fn completed_turn_with_retained_admission(
        store: &StoreBundle,
        session_id: SessionId,
    ) -> (AdmissionId, TurnId) {
        let repository = store.session_repo();
        let (admission_id, turn_id) = active_turn(store, session_id).await;
        let target = repository
            .captured_running_terminal_target(session_id)
            .await
            .expect("capture terminal target")
            .expect("running terminal target");
        assert!(
            repository
                .terminalize_captured_running_session_with_protocol_event(
                    session_id,
                    &completed_terminal(session_id),
                    target,
                )
                .await
                .expect("terminalize while retaining admission")
        );
        (admission_id, turn_id)
    }

    fn delete_terminal_runtime_event_for_corruption_test(
        store: &StoreBundle,
        session_id: SessionId,
        turn_id: TurnId,
    ) {
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "DELETE FROM protocol_runtime_events
                 WHERE session_id = ?1
                   AND turn_id = ?2
                   AND json_extract(msg_json, '$.kind') = 'turn_terminal'",
                params![session_id.to_string(), turn_id.to_string()],
            )
            .expect("delete terminal corruption fixture");
    }

    fn inject_duplicate_terminal_runtime_event_for_corruption_test(
        store: &StoreBundle,
        session_id: SessionId,
        turn_id: TurnId,
    ) {
        let duplicate = project_protocol_run_event(
            &failed_terminal(session_id, "duplicate terminal"),
            Some(session_id),
            turn_id,
            100,
        )
        .expect("duplicate projection")
        .runtime_event;
        let duplicate_json = serde_json::to_string(&duplicate.msg).expect("duplicate JSON");
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute_batch("DROP INDEX idx_protocol_runtime_events_unique_turn_terminal")
            .expect("remove unique terminal index for corruption fixture");
        connection
            .execute(
                "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'corrupt-fixture', ?6)",
                params![
                    duplicate.id.to_string(),
                    session_id.to_string(),
                    turn_id.to_string(),
                    duplicate.sequence_no,
                    duplicate_json,
                    duplicate.created_at_ms,
                ],
            )
            .expect("inject duplicate terminal");
    }

    fn failed_terminal(session_id: SessionId, error: &str) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Failed {
                    error: error.to_string(),
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    /// Builds the historical V48 state explicitly. Current runtime completion never creates
    /// `completed_early`; this fixture exists only to keep backward-compatible reads and cleanup
    /// of already-persisted rows covered.
    fn replace_current_handoff_with_legacy_completed_early(
        store: &StoreBundle,
        agent_session_id: SessionId,
        agent_turn_id: TurnId,
        parent_session_id: SessionId,
    ) {
        let repository = store.session_repo();
        let mut connection = repository.connection.lock().expect("sqlite mutex");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("legacy completed-early fixture transaction");
        let mailbox_id = transaction
            .query_row(
                "SELECT parent_history_item_id
                 FROM agent_completion_handoffs
                 WHERE child_session_id = ?1 AND child_turn_id = ?2",
                params![agent_session_id.to_string(), agent_turn_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .expect("current completion handoff query")
            .expect("current completion handoff");
        transaction
            .execute(
                "DELETE FROM agent_completion_handoffs
                 WHERE child_session_id = ?1 AND child_turn_id = ?2",
                params![agent_session_id.to_string(), agent_turn_id.to_string()],
            )
            .expect("remove current completion handoff");
        transaction
            .execute(
                "DELETE FROM protocol_item_append_order
                 WHERE session_id = ?1
                   AND source_kind = 'mailbox_message'
                   AND source_id = ?2",
                params![parent_session_id.to_string(), mailbox_id],
            )
            .expect("remove current mailbox append order");
        transaction
            .execute(
                "DELETE FROM agent_mailbox_messages WHERE id = ?1",
                params![mailbox_id],
            )
            .expect("remove current mailbox");
        insert_deferred_agent_completion_in_transaction(
            &transaction,
            agent_session_id,
            agent_turn_id,
            parent_session_id,
            DeferredAgentCompletionKind::CompletedEarly,
            normalize_run_lease_now_ms(SystemClock::now_ms()),
        )
        .expect("insert historical completed-early receipt");
        transaction
            .commit()
            .expect("commit historical completed-early fixture");
    }

    fn stored_admission_state(
        store: &StoreBundle,
        session_id: SessionId,
    ) -> (String, Option<String>, Option<String>, Option<i64>) {
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT status, active_run_id, active_turn_id, active_run_lease_expires_at_ms
                 FROM sessions WHERE id = ?1",
                params![session_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .expect("stored admission state")
    }

    fn protocol_turn_table_counts(
        store: &StoreBundle,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Vec<(&'static str, i64)> {
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        [
            "protocol_runtime_events",
            "protocol_history_items",
            "protocol_turn_items",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ]
        .into_iter()
        .map(|table| {
            let count = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1 AND turn_id = ?2"),
                    params![session_id.to_string(), turn_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("protocol table count");
            (table, count)
        })
        .collect()
    }

    fn text_user_turn(turn_id: TurnId, text: &str) -> UserTurn {
        UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: text.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        }
    }

    #[tokio::test]
    async fn child_terminal_and_immediate_parent_final_commit_once_with_exact_response() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let exact_response_id = record_text_response(
            &store,
            child.id,
            child_admission_id,
            child_turn_id,
            "  exact child result  ",
        )
        .await;
        record_text_response(
            &store,
            child.id,
            child_admission_id,
            child_turn_id,
            "unrelated later assistant",
        )
        .await;

        let terminal = completed_terminal_for_response(child.id, Some(exact_response_id));
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &terminal,
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("atomic child terminal"),
            AdmittedTerminalCommit::Applied
        );

        let handoff = store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("handoff query")
            .expect("handoff receipt");
        assert_eq!(handoff.parent_session_id, root_session_id);
        assert_eq!(handoff.parent_agent_path.as_str(), "/root");
        assert!(
            store
                .protocol_event_store()
                .list_history_items(root_session_id, root_turn_id)
                .expect("parent history before safe delivery")
                .into_iter()
                .all(|item| item.id != handoff.history_item_id)
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    root_admission_id,
                    &completed_terminal_for_response(root_session_id, None),
                    root_turn_id,
                    None,
                    None,
                )
                .await
                .expect("parent terminal with finish-drain"),
            AdmittedTerminalCommit::Applied
        );
        let final_items = store
            .protocol_event_store()
            .list_history_items(root_session_id, root_turn_id)
            .expect("parent turn history")
            .into_iter()
            .filter(|item| {
                matches!(
                    &item.payload,
                    HistoryItemPayload::InterAgentCommunication { communication }
                        if communication.author == "/root/child"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(final_items.len(), 1);
        assert_eq!(final_items[0].id, handoff.history_item_id);
        let HistoryItemPayload::InterAgentCommunication { communication } = &final_items[0].payload
        else {
            unreachable!("filtered parent FINAL");
        };
        assert_eq!(
            communication.content,
            "Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/child\nPayload:\n  exact child result  "
        );
        assert!(!communication.content.contains("unrelated later assistant"));

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &terminal,
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("duplicate child terminal attempt"),
            AdmittedTerminalCommit::NotOwned
        );
        let session_repo = store.session_repo();
        let connection = session_repo.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM agent_completion_handoffs",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .expect("receipt count"),
            1
        );
        drop(connection);

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    root_admission_id,
                    &completed_terminal_for_response(root_session_id, None),
                    root_turn_id,
                    None,
                    None,
                )
                .await
                .expect("parent terminal after durable child FINAL"),
            AdmittedTerminalCommit::NotOwned
        );
        let rollback_error = store
            .session_repo()
            .rollback_session_transaction(child.id, 1)
            .await
            .expect_err("handoff participant rollback must be rejected");
        assert!(
            rollback_error
                .to_string()
                .contains("durable agent completion handoff")
        );
        let deleted = store
            .session_repo()
            .delete_session_tree(root_session_id)
            .await
            .expect("leaf-first agent tree delete");
        assert_eq!(deleted.len(), 2);
        assert_eq!(
            store
                .session_repo()
                .connection
                .lock()
                .expect("sqlite mutex")
                .query_row(
                    "SELECT COUNT(*) FROM agent_completion_handoffs",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .expect("receipt count after tree delete"),
            0
        );
    }

    #[tokio::test]
    async fn oversized_completed_child_handoff_preserves_the_exact_result_and_receipt_identity() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let oversized = format!(
            "completed-result-head:{}:completed-result-tail",
            "0123456789".repeat(1_200)
        );
        let response_id = record_text_response(
            &store,
            child.id,
            child_admission_id,
            child_turn_id,
            &oversized,
        )
        .await;

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, Some(response_id)),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("oversized child terminal"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("handoff query")
            .expect("handoff receipt");
        assert_eq!(handoff.child_session_id, child.id);
        assert_eq!(handoff.child_turn_id, child_turn_id);
        assert_eq!(handoff.parent_session_id, root_session_id);
        assert_eq!(handoff.parent_agent_path.as_str(), "/root");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    root_session_id,
                    root_admission_id,
                    root_turn_id,
                    128,
                )
                .expect("safe oversized completion delivery")
                .history_item_ids,
            vec![handoff.history_item_id]
        );

        let items = store
            .protocol_event_store()
            .history_items_by_id(root_session_id, &[handoff.history_item_id])
            .expect("exact receipt history");
        let [item] = items.as_slice() else {
            panic!("completion receipt must reference one exact parent history item");
        };
        assert_eq!(item.id, handoff.history_item_id);
        let HistoryItemPayload::InterAgentCommunication { communication } = &item.payload else {
            panic!("completion receipt must reference inter-agent communication");
        };
        assert_eq!(communication.author, "/root/child");
        assert_eq!(communication.recipient, "/root");
        let (_, payload) = communication
            .content
            .split_once("Payload:\n")
            .expect("completion envelope");
        assert_eq!(payload, oversized);
    }

    #[tokio::test]
    async fn oversized_failed_child_error_is_middle_truncated_for_the_exact_immediate_parent() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                child.id,
                grandchild.id,
                "/root/child/grandchild",
                "grandchild",
            )
            .await
            .expect("grandchild edge");
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let (grandchild_admission_id, grandchild_turn_id) =
            active_turn(&store, grandchild.id).await;
        let oversized_error = format!(
            "failed-error-head:{}:failed-error-tail",
            "abcdefghij".repeat(1_200)
        );

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    grandchild.id,
                    grandchild_admission_id,
                    &failed_terminal(grandchild.id, &oversized_error),
                    grandchild_turn_id,
                    None,
                    None,
                )
                .await
                .expect("oversized grandchild failure terminal"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(grandchild.id, grandchild_turn_id)
            .expect("handoff query")
            .expect("handoff receipt");
        assert_eq!(handoff.child_session_id, grandchild.id);
        assert_eq!(handoff.child_turn_id, grandchild_turn_id);
        assert_eq!(handoff.parent_session_id, child.id);
        assert_eq!(handoff.parent_agent_path.as_str(), "/root/child");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    child.id,
                    child_admission_id,
                    child_turn_id,
                    128,
                )
                .expect("safe oversized failure delivery")
                .history_item_ids,
            vec![handoff.history_item_id]
        );

        let items = store
            .protocol_event_store()
            .history_items_by_id(child.id, &[handoff.history_item_id])
            .expect("exact receipt history");
        let [item] = items.as_slice() else {
            panic!("failure receipt must reference one exact parent history item");
        };
        assert_eq!(item.id, handoff.history_item_id);
        let HistoryItemPayload::InterAgentCommunication { communication } = &item.payload else {
            panic!("failure receipt must reference inter-agent communication");
        };
        assert_eq!(communication.author, "/root/child/grandchild");
        assert_eq!(communication.recipient, "/root/child");
        let (_, payload) = communication
            .content
            .split_once("Payload:\n")
            .expect("completion envelope");
        let error = payload
            .strip_prefix("Agent errored: ")
            .and_then(|payload| {
                payload.strip_suffix(&format!("\n\n{AGENT_COMPLETION_ERROR_NEXT_ACTION}"))
            })
            .expect("bounded Codex error payload");
        assert_ne!(error, oversized_error);
        assert!(error.starts_with("failed-error-head:"));
        assert!(error.ends_with(":failed-error-tail"));
        assert!(error.contains(" tokens truncated…"));
        assert!(
            crate::context::context_window::estimate_text_tokens(error)
                <= AGENT_COMPLETION_ERROR_MAX_TOKENS
        );
        assert!(
            crate::context::context_window::estimate_text_tokens(payload)
                <= AGENT_COMPLETION_MESSAGE_MAX_TOKENS
        );
    }

    #[tokio::test]
    async fn completion_handoff_failure_rolls_back_terminal_and_parent_final_together() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_, root_turn_id) = active_turn(&store, root_session_id).await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let response_id = record_text_response(
            &store,
            child.id,
            child_admission_id,
            child_turn_id,
            "must commit atomically",
        )
        .await;
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER abort_agent_completion_receipt
                 BEFORE INSERT ON agent_completion_handoffs
                 BEGIN
                     SELECT RAISE(ABORT, 'injected completion receipt failure');
                 END;",
            )
            .expect("failure trigger");

        let error = store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, Some(response_id)),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect_err("receipt failure must abort whole terminal transaction");
        assert!(
            error
                .to_string()
                .contains("injected completion receipt failure")
        );
        assert_eq!(stored_admission_state(&store, child.id).0, "running");
        assert!(
            terminal_for_turn_in_connection(
                &store
                    .session_repo()
                    .connection
                    .lock()
                    .expect("sqlite mutex"),
                child.id,
                child_turn_id,
            )
            .expect("terminal lookup after rollback")
            .is_none()
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(root_session_id, root_turn_id)
                .expect("parent history after rollback")
                .into_iter()
                .all(|item| !matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                ))
        );
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch("DROP TRIGGER abort_agent_completion_receipt;")
            .expect("drop failure trigger");

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, Some(response_id)),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("retry terminal transaction"),
            AdmittedTerminalCommit::Applied
        );
    }

    #[tokio::test]
    async fn nested_failure_handoff_targets_stale_immediate_parent_session_scope() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                child.id,
                grandchild.id,
                "/root/child/grandchild",
                "grandchild",
            )
            .await
            .expect("grandchild edge");
        store
            .session_repo()
            .admit_session_turn_at(child.id, TurnId::new(), 1, 1)
            .await
            .expect("stale parent admission")
            .expect("stale parent admitted");
        let (grandchild_admission_id, grandchild_turn_id) =
            active_turn(&store, grandchild.id).await;

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    grandchild.id,
                    grandchild_admission_id,
                    &failed_terminal(grandchild.id, "bounded failure"),
                    grandchild_turn_id,
                    None,
                    None,
                )
                .await
                .expect("grandchild failure terminal"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(grandchild.id, grandchild_turn_id)
            .expect("nested handoff query")
            .expect("nested handoff");
        assert_eq!(handoff.parent_session_id, child.id);
        assert_eq!(handoff.parent_agent_path.as_str(), "/root/child");
        assert_eq!(
            store
                .session_repo()
                .schedulable_owner_resume_request_id(child.id)
                .expect("expired parent continuation"),
            None,
            "an expired running parent is recovered by the runtime owner before an OwnerResume can be scheduled"
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(child.id)
                .expect("child canonical history before safe delivery")
                .into_iter()
                .all(|item| item.id != handoff.history_item_id)
        );
        let child_mail = store
            .session_repo()
            .agent_mailbox_communications_by_id(child.id, &[handoff.history_item_id])
            .expect("queued nested failure");
        assert!(matches!(
            child_mail.as_slice(),
            [(id, communication)]
                if *id == handoff.history_item_id
                    && communication.content
                        == "Message Type: FINAL_ANSWER\nTask name: /root/child\nSender: /root/child/grandchild\nPayload:\nAgent errored: bounded failure\n\nThis agent's turn failed. If you still need this agent, use the available collaboration tools to give it another task."
        ));
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(root_session_id)
                .expect("root canonical history")
                .into_iter()
                .all(|item| !matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                ))
        );
    }

    #[tokio::test]
    async fn completed_child_without_a_response_delivers_an_empty_final_payload() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (admission_id, turn_id) = active_turn(&store, child.id).await;

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    admission_id,
                    &completed_terminal_for_response(child.id, None),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("empty child completion"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(child.id, turn_id)
            .expect("handoff query")
            .expect("empty completion handoff");
        let root_turn_id = TurnId::new();
        let root_admission = store
            .session_repo()
            .admit_session_turn(root_session_id, root_turn_id)
            .await
            .expect("root safe-delivery admission")
            .expect("root admitted");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    root_session_id,
                    root_admission.admission_id,
                    root_turn_id,
                    128,
                )
                .expect("safe empty completion delivery")
                .history_item_ids,
            vec![handoff.history_item_id]
        );
        let final_content = store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("root history")
            .into_iter()
            .find_map(|item| match item.payload {
                HistoryItemPayload::InterAgentCommunication { communication }
                    if item.id == handoff.history_item_id =>
                {
                    Some(communication.content)
                }
                _ => None,
            })
            .expect("empty completion FINAL");
        assert_eq!(
            final_content,
            "Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/child\nPayload:\n"
        );
    }

    #[tokio::test]
    async fn completed_child_does_not_revive_a_cancelled_immediate_parent() {
        let (store, root_session_id) = test_repo().await;
        let child = create_sibling_session(&store, root_session_id, "child").await;
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let interrupted_root = RunEvent::TurnTerminal {
            session_id: root_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    root_session_id,
                    root_admission_id,
                    &interrupted_root,
                    root_turn_id,
                    None,
                    None,
                )
                .await
                .expect("cancel parent"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, None),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("complete child after parent cancellation"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &tree_stopped_terminal(child.id),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("late tree-stop terminal"),
            AdmittedTerminalCommit::NotOwned
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("handoff query")
            .expect("completed child FINAL");
        let parent_finals = store
            .session_repo()
            .agent_mailbox_communications_by_id(root_session_id, &[handoff.history_item_id])
            .expect("queued parent FINAL");
        assert!(matches!(
            parent_finals.as_slice(),
            [(_, communication)] if !communication.trigger_turn
        ));
        assert!(
            store
                .session_repo()
                .schedulable_owner_resume_request_id(root_session_id)
                .expect("cancelled parent owner resume")
                .is_none()
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(root_session_id)
                .expect("root history")
                .into_iter()
                .all(|item| !matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                ))
        );
    }

    #[tokio::test]
    async fn new_and_resumed_turns_admit_run_and_turn_as_one_owner() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let first_turn_id = TurnId::new();
        let first_admission_id = repository
            .admit_session_turn(session_id, first_turn_id)
            .await
            .expect("first admission")
            .expect("first turn admitted")
            .admission_id;

        let first_state = stored_admission_state(&store, session_id);
        assert_eq!(first_state.0, "running");
        assert_eq!(first_state.1, Some(first_admission_id.to_string()));
        assert_eq!(first_state.2, Some(first_turn_id.to_string()));
        assert!(first_state.3.is_some());

        let terminal = completed_terminal(session_id);
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    first_admission_id,
                    &terminal,
                    first_turn_id,
                    None,
                    None,
                )
                .await
                .expect("terminal commit"),
            AdmittedTerminalCommit::Applied
        );

        let resumed_turn_id = TurnId::new();
        let resumed_admission_id = repository
            .admit_session_turn(session_id, resumed_turn_id)
            .await
            .expect("resumed admission")
            .expect("resumed turn admitted")
            .admission_id;
        let resumed_state = stored_admission_state(&store, session_id);
        assert_eq!(resumed_state.0, "running");
        assert_eq!(resumed_state.1, Some(resumed_admission_id.to_string()));
        assert_eq!(resumed_state.2, Some(resumed_turn_id.to_string()));
        assert!(resumed_state.3.is_some());
    }

    #[tokio::test]
    async fn admission_rejects_every_prior_turn_identity_trace() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let (admission_id, used_turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &completed_terminal(session_id),
                    used_turn_id,
                    None,
                    None,
                )
                .await
                .expect("terminalize used turn"),
            AdmittedTerminalCommit::Applied
        );

        let reused_error = repository
            .admit_session_turn(session_id, used_turn_id)
            .await
            .expect_err("canonical turn identity must never be reusable");
        assert!(reused_error.to_string().contains("has already been used"));

        let allocator_only_turn_id = TurnId::new();
        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            connection
                .execute(
                    "INSERT INTO protocol_turn_sequence_allocators
                     (session_id, turn_id, next_sequence_no)
                     VALUES (?1, ?2, 0)",
                    params![session_id.to_string(), allocator_only_turn_id.to_string()],
                )
                .expect("inject orphan allocator trace");
        }
        let allocator_error = repository
            .admit_session_turn(session_id, allocator_only_turn_id)
            .await
            .expect_err("allocator trace must fence turn identity reuse");
        assert!(
            allocator_error
                .to_string()
                .contains("has already been used")
        );
        assert!(
            !repository
                .has_fresh_run_admission(session_id)
                .await
                .expect("no admission after collisions")
        );
    }

    #[tokio::test]
    async fn terminal_lease_outcome_is_exact_to_the_requested_nonreusable_turn() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let (first_admission_id, first_turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    first_admission_id,
                    &completed_terminal(session_id),
                    first_turn_id,
                    None,
                    None,
                )
                .await
                .expect("complete first turn"),
            AdmittedTerminalCommit::Applied
        );
        let (second_admission_id, second_turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    second_admission_id,
                    &failed_terminal(session_id, "second failed"),
                    second_turn_id,
                    None,
                    None,
                )
                .await
                .expect("fail second turn"),
            AdmittedTerminalCommit::Applied
        );

        assert!(matches!(
            repository
                .renew_admitted_run_lease(session_id, first_admission_id, first_turn_id)
                .await
                .expect("first terminal lease outcome"),
            RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                if terminal.session_status() == SessionStatus::Completed
        ));
        assert!(matches!(
            repository
                .renew_admitted_run_lease(session_id, AdmissionId::new(), TurnId::new(),)
                .await
                .expect("unrelated terminal lease outcome"),
            RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
        ));
    }

    #[tokio::test]
    async fn latest_terminal_before_turn_excludes_current_and_uses_append_order() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let (first_admission_id, first_turn_id) = active_turn(&store, session_id).await;
        let mut first_terminal = completed_terminal(session_id);
        let RunEvent::TurnTerminal { terminal, .. } = &mut first_terminal else {
            unreachable!("completed terminal helper must be terminal")
        };
        terminal.change_count = 1;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    first_admission_id,
                    &first_terminal,
                    first_turn_id,
                    Some(900),
                    None,
                )
                .await
                .expect("complete first turn"),
            AdmittedTerminalCommit::Applied
        );

        let (second_admission_id, second_turn_id) = active_turn(&store, session_id).await;
        let mut second_terminal = completed_terminal(session_id);
        let RunEvent::TurnTerminal { terminal, .. } = &mut second_terminal else {
            unreachable!("completed terminal helper must be terminal")
        };
        terminal.change_count = 2;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    second_admission_id,
                    &second_terminal,
                    second_turn_id,
                    Some(1),
                    None,
                )
                .await
                .expect("complete second turn"),
            AdmittedTerminalCommit::Applied
        );

        let (_, current_turn_id) = active_turn(&store, session_id).await;
        let latest = repository
            .latest_durable_terminal_before_turn(session_id, current_turn_id)
            .await
            .expect("latest prior terminal")
            .expect("second terminal");
        assert_eq!(latest.change_count, 2);

        let before_second = repository
            .latest_durable_terminal_before_turn(session_id, second_turn_id)
            .await
            .expect("terminal before second")
            .expect("first terminal");
        assert_eq!(before_second.change_count, 1);
    }

    #[tokio::test]
    async fn retained_terminal_corruption_cannot_be_hidden_or_release_its_owner() {
        let (renew_store, renew_session_id) = test_repo().await;
        let renew_repository = renew_store.session_repo();
        let (renew_admission_id, renew_turn_id) =
            completed_turn_with_retained_admission(&renew_store, renew_session_id).await;
        delete_terminal_runtime_event_for_corruption_test(
            &renew_store,
            renew_session_id,
            renew_turn_id,
        );
        let renew_before = stored_admission_state(&renew_store, renew_session_id);
        let status_error = renew_repository
            .admitted_run_status_at(
                renew_session_id,
                renew_admission_id,
                renew_turn_id,
                SystemClock::now_ms(),
            )
            .await
            .expect_err("single-session reader must reject a missing retained terminal");
        assert!(
            status_error
                .to_string()
                .contains("without a durable terminal")
        );
        let renewal_error = renew_repository
            .renew_admitted_run_lease(renew_session_id, AdmissionId::new(), TurnId::new())
            .await
            .expect_err("wrong caller must not hide a missing retained terminal");
        assert!(
            renewal_error
                .to_string()
                .contains("without a durable terminal")
        );
        assert_eq!(
            stored_admission_state(&renew_store, renew_session_id),
            renew_before
        );
        let wrong_release_error = renew_repository
            .release_stopped_run_admission(renew_session_id, AdmissionId::new())
            .await
            .expect_err("wrong release caller must not hide a missing retained terminal");
        assert!(
            wrong_release_error
                .to_string()
                .contains("without a durable terminal")
        );
        assert_eq!(
            stored_admission_state(&renew_store, renew_session_id),
            renew_before
        );

        let (release_store, release_session_id) = test_repo().await;
        let release_repository = release_store.session_repo();
        let (release_admission_id, release_turn_id) =
            completed_turn_with_retained_admission(&release_store, release_session_id).await;
        delete_terminal_runtime_event_for_corruption_test(
            &release_store,
            release_session_id,
            release_turn_id,
        );
        let release_before = stored_admission_state(&release_store, release_session_id);
        let release_error = release_repository
            .release_stopped_run_admission(release_session_id, release_admission_id)
            .await
            .expect_err("release must validate the retained terminal first");
        assert!(
            release_error
                .to_string()
                .contains("without a durable terminal")
        );
        assert_eq!(
            stored_admission_state(&release_store, release_session_id),
            release_before
        );

        let (recovery_store, recovery_session_id) = test_repo().await;
        let recovery_repository = recovery_store.session_repo();
        let (recovery_admission_id, recovery_turn_id) =
            completed_turn_with_retained_admission(&recovery_store, recovery_session_id).await;
        recovery_repository
            .inject_raw_runtime_state_for_corruption_test(
                recovery_session_id,
                "completed",
                Some(&recovery_admission_id.to_string()),
                Some(&recovery_turn_id.to_string()),
                Some(1),
            )
            .expect("expire retained terminal owner");
        delete_terminal_runtime_event_for_corruption_test(
            &recovery_store,
            recovery_session_id,
            recovery_turn_id,
        );
        let recovery_before = stored_admission_state(&recovery_store, recovery_session_id);
        let replacement_turn_id = TurnId::new();
        let recovery_error = recovery_repository
            .admit_session_turn_at(
                recovery_session_id,
                replacement_turn_id,
                2,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect_err("expired recovery must validate the retained terminal first");
        assert!(
            recovery_error
                .to_string()
                .contains("without a durable terminal")
        );
        assert_eq!(
            stored_admission_state(&recovery_store, recovery_session_id),
            recovery_before
        );
        assert!(
            recovery_repository
                .durable_terminal_for_turn(recovery_session_id, replacement_turn_id)
                .await
                .expect("replacement terminal lookup")
                .is_none()
        );
        let same_turn_error = recovery_repository
            .admit_session_turn_at(
                recovery_session_id,
                recovery_turn_id,
                2,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect_err("corrupt retained identity must not become reusable");
        assert!(
            same_turn_error
                .to_string()
                .contains("without a durable terminal")
        );
        assert_eq!(
            stored_admission_state(&recovery_store, recovery_session_id),
            recovery_before
        );

        let (mismatch_store, mismatch_session_id) = test_repo().await;
        let mismatch_repository = mismatch_store.session_repo();
        let (mismatch_admission_id, mismatch_turn_id) =
            completed_turn_with_retained_admission(&mismatch_store, mismatch_session_id).await;
        let mismatch_lease = stored_admission_state(&mismatch_store, mismatch_session_id)
            .3
            .expect("retained lease");
        mismatch_repository
            .inject_raw_runtime_state_for_corruption_test(
                mismatch_session_id,
                "failed",
                Some(&mismatch_admission_id.to_string()),
                Some(&mismatch_turn_id.to_string()),
                Some(mismatch_lease),
            )
            .expect("inject terminal status mismatch");
        let mismatch_before = stored_admission_state(&mismatch_store, mismatch_session_id);
        let mismatch_status_error = mismatch_repository
            .admitted_run_status_at(
                mismatch_session_id,
                mismatch_admission_id,
                mismatch_turn_id,
                SystemClock::now_ms(),
            )
            .await
            .expect_err("single-session reader must reject a terminal status mismatch");
        assert!(
            mismatch_status_error
                .to_string()
                .contains("contradicts durable terminal status")
        );
        let mismatch_error = mismatch_repository
            .renew_admitted_run_lease(mismatch_session_id, AdmissionId::new(), TurnId::new())
            .await
            .expect_err("wrong caller must not hide a terminal status mismatch");
        assert!(
            mismatch_error
                .to_string()
                .contains("contradicts durable terminal status")
        );
        assert_eq!(
            stored_admission_state(&mismatch_store, mismatch_session_id),
            mismatch_before
        );
        let mismatch_release_error = mismatch_repository
            .release_stopped_run_admission(mismatch_session_id, mismatch_admission_id)
            .await
            .expect_err("release must reject a terminal status mismatch");
        assert!(
            mismatch_release_error
                .to_string()
                .contains("contradicts durable terminal status")
        );
        assert_eq!(
            stored_admission_state(&mismatch_store, mismatch_session_id),
            mismatch_before
        );
        mismatch_repository
            .inject_raw_runtime_state_for_corruption_test(
                mismatch_session_id,
                "failed",
                Some(&mismatch_admission_id.to_string()),
                Some(&mismatch_turn_id.to_string()),
                Some(1),
            )
            .expect("expire mismatched terminal owner");
        let expired_mismatch_before = stored_admission_state(&mismatch_store, mismatch_session_id);
        let mismatch_recovery_error = mismatch_repository
            .admit_session_turn_at(
                mismatch_session_id,
                TurnId::new(),
                2,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect_err("expired recovery must reject a terminal status mismatch");
        assert!(
            mismatch_recovery_error
                .to_string()
                .contains("contradicts durable terminal status")
        );
        assert_eq!(
            stored_admission_state(&mismatch_store, mismatch_session_id),
            expired_mismatch_before
        );
    }

    #[tokio::test]
    async fn duplicate_retained_terminal_blocks_renewal_release_and_recovery() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let (admission_id, turn_id) =
            completed_turn_with_retained_admission(&store, session_id).await;
        inject_duplicate_terminal_runtime_event_for_corruption_test(&store, session_id, turn_id);
        let before = stored_admission_state(&store, session_id);

        let status_error = repository
            .admitted_run_status_at(session_id, admission_id, turn_id, SystemClock::now_ms())
            .await
            .expect_err("single-session reader must reject duplicate terminals");
        assert!(
            status_error
                .to_string()
                .contains("multiple durable terminals")
        );

        let renewal_error = repository
            .renew_admitted_run_lease(session_id, AdmissionId::new(), TurnId::new())
            .await
            .expect_err("duplicate terminal must be detected before owner comparison");
        assert!(
            renewal_error
                .to_string()
                .contains("multiple durable terminals")
        );
        assert_eq!(stored_admission_state(&store, session_id), before);

        let release_error = repository
            .release_stopped_run_admission(session_id, admission_id)
            .await
            .expect_err("duplicate terminal must block owner release");
        assert!(
            release_error
                .to_string()
                .contains("multiple durable terminals")
        );
        assert_eq!(stored_admission_state(&store, session_id), before);

        repository
            .inject_raw_runtime_state_for_corruption_test(
                session_id,
                "completed",
                Some(&admission_id.to_string()),
                Some(&turn_id.to_string()),
                Some(1),
            )
            .expect("expire duplicate terminal owner");
        let expired_before = stored_admission_state(&store, session_id);
        let recovery_error = repository
            .admit_session_turn_at(
                session_id,
                TurnId::new(),
                2,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect_err("duplicate terminal must block expired-owner recovery");
        assert!(
            recovery_error
                .to_string()
                .contains("multiple durable terminals")
        );
        assert_eq!(stored_admission_state(&store, session_id), expired_before);
    }

    #[tokio::test]
    async fn valid_retained_terminal_can_be_observed_released_or_replaced() {
        let (release_store, release_session_id) = test_repo().await;
        let release_repository = release_store.session_repo();
        let (release_admission_id, release_turn_id) =
            completed_turn_with_retained_admission(&release_store, release_session_id).await;
        assert!(matches!(
            release_repository
                .renew_admitted_run_lease(
                    release_session_id,
                    release_admission_id,
                    release_turn_id,
                )
                .await
                .expect("typed retained terminal"),
            RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                if terminal.session_status() == SessionStatus::Completed
        ));
        assert!(
            release_repository
                .release_stopped_run_admission(release_session_id, release_admission_id)
                .await
                .expect("release valid retained terminal")
        );
        assert_eq!(
            stored_admission_state(&release_store, release_session_id),
            ("completed".to_string(), None, None, None)
        );

        let (replace_store, replace_session_id) = test_repo().await;
        let replace_repository = replace_store.session_repo();
        let (replace_admission_id, replace_turn_id) =
            completed_turn_with_retained_admission(&replace_store, replace_session_id).await;
        replace_repository
            .inject_raw_runtime_state_for_corruption_test(
                replace_session_id,
                "completed",
                Some(&replace_admission_id.to_string()),
                Some(&replace_turn_id.to_string()),
                Some(1),
            )
            .expect("expire valid retained terminal owner");
        let replacement_turn_id = TurnId::new();
        let replacement = replace_repository
            .admit_session_turn_at(
                replace_session_id,
                replacement_turn_id,
                2,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect("replace valid expired retained owner")
            .expect("replacement admission");
        let replaced_state = stored_admission_state(&replace_store, replace_session_id);
        assert_eq!(replaced_state.0, "running");
        assert_eq!(replaced_state.1, Some(replacement.admission_id.to_string()));
        assert_eq!(replaced_state.2, Some(replacement_turn_id.to_string()));
    }

    #[tokio::test]
    async fn running_session_with_a_terminal_is_reported_as_corrupt() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let project_id = repository
            .get_session(session_id)
            .await
            .expect("session before corruption")
            .project_id;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let terminal_event = completed_terminal(session_id);
        let projection = project_protocol_run_event(&terminal_event, Some(session_id), turn_id, 1)
            .expect("terminal projection");
        {
            let mut connection = repository.connection.lock().expect("sqlite mutex");
            let transaction = connection.transaction().expect("transaction");
            insert_session_owned_event_bundle_in_transaction(
                &SESSION_PROTOCOL_WRITE_AUTHORITY,
                &transaction,
                &projection.runtime_event,
                projection.history_item.as_ref(),
                projection.turn_item.as_ref(),
            )
            .expect("inject terminal before status CAS");
            transaction.commit().expect("commit corrupt fixture");
        }
        let corrupt_state = stored_admission_state(&store, session_id);
        let protocol_before = (
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .expect("history before rejected write")
                .len(),
            store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("runtime before rejected write")
                .len(),
            store
                .protocol_event_store()
                .list_turn_items(session_id, turn_id)
                .expect("turn items before rejected write")
                .len(),
        );

        let response_error = repository
            .record_model_response_with_protocol_bundle(
                session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id: ModelResponseId::new(),
                    assistant_text: Some("must not commit after terminal".to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: Vec::new(),
                },
            )
            .await
            .expect_err("active-admission writer must reject running plus terminal corruption");
        assert!(
            response_error
                .to_string()
                .contains("already has a durable terminal")
        );
        let protocol_after = (
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .expect("history after rejected write")
                .len(),
            store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("runtime after rejected write")
                .len(),
            store
                .protocol_event_store()
                .list_turn_items(session_id, turn_id)
                .expect("turn items after rejected write")
                .len(),
        );
        assert_eq!(protocol_after, protocol_before);
        assert!(repository.get_session(session_id).await.is_err());
        assert!(
            repository
                .active_session_for_project(project_id)
                .await
                .is_err()
        );
        assert!(
            repository
                .mutation_blocker_in_session_tree(session_id)
                .await
                .is_err()
        );

        let admission_error = repository
            .admit_session_turn(session_id, TurnId::new())
            .await
            .expect_err("running terminal must fail admission integrity checks");
        assert!(
            admission_error
                .to_string()
                .contains("already has a durable terminal")
        );
        assert_eq!(stored_admission_state(&store, session_id), corrupt_state);

        let renewal_error = repository
            .renew_admitted_run_lease(session_id, admission_id, turn_id)
            .await
            .expect_err("running terminal must fail renewal integrity checks");
        assert!(
            renewal_error
                .to_string()
                .contains("already has a durable terminal")
        );
        let release_error = repository
            .release_stopped_run_admission(session_id, admission_id)
            .await
            .expect_err("running terminal must fail release integrity checks");
        assert!(
            release_error
                .to_string()
                .contains("already has a durable terminal")
        );
        assert_eq!(stored_admission_state(&store, session_id), corrupt_state);
        let terminal_error = repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &terminal_event,
                turn_id,
                None,
                None,
            )
            .await
            .expect_err("running terminal must not masquerade as an idempotent commit");
        assert!(
            terminal_error
                .to_string()
                .contains("already has a durable terminal")
        );
    }

    #[tokio::test]
    async fn terminal_reader_rejects_multiple_rows_even_if_the_index_is_corrupted() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &completed_terminal(session_id),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("first terminal"),
            AdmittedTerminalCommit::Applied
        );
        let duplicate = project_protocol_run_event(
            &failed_terminal(session_id, "duplicate terminal"),
            Some(session_id),
            turn_id,
            100,
        )
        .expect("duplicate projection")
        .runtime_event;
        let duplicate_json = serde_json::to_string(&duplicate.msg).expect("duplicate JSON");
        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            connection
                .execute_batch("DROP INDEX idx_protocol_runtime_events_unique_turn_terminal")
                .expect("remove index for corruption fixture");
            connection
                .execute(
                    "INSERT INTO protocol_runtime_events
                     (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'corrupt-fixture', ?6)",
                    params![
                        duplicate.id.to_string(),
                        session_id.to_string(),
                        turn_id.to_string(),
                        duplicate.sequence_no,
                        duplicate_json,
                        duplicate.created_at_ms,
                    ],
                )
                .expect("inject duplicate terminal");
        }

        let error = repository
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect_err("duplicate durable terminal must fail closed");
        assert!(error.to_string().contains("multiple durable terminals"));
    }

    #[tokio::test]
    async fn initial_user_turn_validation_is_side_effect_free() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let admission_turn_id = TurnId::new();
        let mismatched_turn = text_user_turn(TurnId::new(), "mismatched");

        let mismatch = repository
            .admit_session_turn_with_initial_user_turn(
                session_id,
                admission_turn_id,
                Some(&mismatched_turn),
            )
            .await
            .expect_err("mismatched user turn must be rejected");
        assert!(mismatch.to_string().contains("identity mismatch"));

        let empty_turn = UserTurn {
            turn_id: admission_turn_id,
            items: vec![UserInputItem::Text {
                text: " \n ".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        };
        let empty = repository
            .admit_session_turn_with_initial_user_turn(
                session_id,
                admission_turn_id,
                Some(&empty_turn),
            )
            .await
            .expect_err("empty user turn must be rejected");
        assert!(empty.to_string().contains("must contain text or an image"));

        assert_eq!(
            stored_admission_state(&store, session_id),
            ("idle".to_string(), None, None, None)
        );
        assert!(
            protocol_turn_table_counts(&store, session_id, admission_turn_id)
                .iter()
                .all(|(_, count)| *count == 0)
        );
    }

    #[tokio::test]
    async fn admission_bundle_rolls_back_session_goal_protocol_and_allocator_on_projection_failure()
    {
        for failure_point in ["session_started", "user_turn"] {
            let (store, session_id) = test_repo().await;
            let repository = store.session_repo();
            let turn_id = TurnId::new();
            let user_turn = text_user_turn(turn_id, "atomic request");
            let trigger = match failure_point {
                "session_started" => {
                    "CREATE TRIGGER abort_initial_session_started
                     BEFORE INSERT ON protocol_runtime_events
                     WHEN NEW.sequence_no = 0
                     BEGIN SELECT RAISE(ABORT, 'injected SessionStarted failure'); END;"
                }
                "user_turn" => {
                    "CREATE TRIGGER abort_initial_user_turn
                     BEFORE INSERT ON protocol_history_items
                     WHEN json_extract(NEW.payload_json, '$.kind') = 'user_turn'
                     BEGIN SELECT RAISE(ABORT, 'injected UserTurnStored failure'); END;"
                }
                _ => unreachable!("known failure point"),
            };
            repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .execute_batch(trigger)
                .expect("failure trigger");

            repository
                .admit_session_turn_with_goal_objective_and_initial_user_turn(
                    session_id,
                    turn_id,
                    "must roll back",
                    Some(&user_turn),
                )
                .await
                .expect_err("injected projection failure");

            assert_eq!(
                stored_admission_state(&store, session_id),
                ("idle".to_string(), None, None, None),
                "{failure_point} retained the session admission"
            );
            assert!(
                repository
                    .get_thread_goal(session_id)
                    .await
                    .expect("goal after rollback")
                    .is_none(),
                "{failure_point} retained the goal mutation"
            );
            for (table, count) in protocol_turn_table_counts(&store, session_id, turn_id) {
                assert_eq!(
                    count, 0,
                    "{failure_point} retained a row in {table} after rollback"
                );
            }
        }
    }

    #[tokio::test]
    async fn image_only_initial_user_turn_is_committed_with_the_admission() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let turn_id = TurnId::new();
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Image {
                image: crate::session::ImagePart {
                    source_path: None,
                    mime_type: "image/png".to_string(),
                    data_base64: "iVBORw0KGgo=".to_string(),
                    byte_len: 8,
                },
            }],
            prompt_dispatch: None,
            editor_context: None,
        };

        repository
            .admit_session_turn_with_initial_user_turn(session_id, turn_id, Some(&user_turn))
            .await
            .expect("image-only admission")
            .expect("image-only turn admitted");

        let events = store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("runtime events");
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence_no)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        let history = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history");
        assert!(matches!(
            history.as_slice(),
            [HistoryItem {
                payload: HistoryItemPayload::UserTurn { content, .. },
                ..
            }] if matches!(content.as_slice(), [ContentPart::Image { .. }])
        ));
    }

    #[tokio::test]
    async fn concurrent_admission_commits_exactly_one_run_and_turn_owner() {
        let (store, session_id) = test_repo().await;
        let first_repository = store.session_repo();
        let second_repository = store.session_repo();
        let first_turn_id = TurnId::new();
        let second_turn_id = TurnId::new();
        let first_user_turn = text_user_turn(first_turn_id, "first");
        let second_user_turn = text_user_turn(second_turn_id, "second");
        let (first, second) = tokio::join!(
            first_repository.admit_session_turn_with_initial_user_turn(
                session_id,
                first_turn_id,
                Some(&first_user_turn),
            ),
            second_repository.admit_session_turn_with_initial_user_turn(
                session_id,
                second_turn_id,
                Some(&second_user_turn),
            ),
        );
        let first = first.expect("first admission attempt");
        let second = second.expect("second admission attempt");
        let (winning_admission_id, winning_turn_id) = match (first, second) {
            (Some(admission), None) => (admission.admission_id, first_turn_id),
            (None, Some(admission)) => (admission.admission_id, second_turn_id),
            outcome => panic!("expected one admitted turn, got {outcome:?}"),
        };

        let state = stored_admission_state(&store, session_id);
        assert_eq!(state.0, "running");
        assert_eq!(state.1, Some(winning_admission_id.to_string()));
        assert_eq!(state.2, Some(winning_turn_id.to_string()));
        assert!(state.3.is_some());
        let losing_turn_id = if winning_turn_id == first_turn_id {
            second_turn_id
        } else {
            first_turn_id
        };
        assert_eq!(
            protocol_turn_table_counts(&store, session_id, winning_turn_id),
            vec![
                ("protocol_runtime_events", 2),
                ("protocol_history_items", 1),
                ("protocol_turn_items", 1),
                ("protocol_item_append_order", 4),
                ("protocol_turn_sequence_allocators", 1),
            ],
            "the winner must own the complete admission bundle"
        );
        assert!(
            protocol_turn_table_counts(&store, session_id, losing_turn_id)
                .iter()
                .all(|(_, count)| *count == 0),
            "the losing turn must leave no partial bundle"
        );
    }

    #[tokio::test]
    async fn goal_change_admission_captures_one_immutable_goal_owner() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        repository
            .replace_thread_goal(
                session_id,
                "old objective",
                ThreadGoalStatus::Active,
                Some(100),
            )
            .await
            .expect("initial goal");
        let (_, original_goal_id) = repository
            .get_thread_goal_with_id(session_id)
            .await
            .expect("read initial goal")
            .expect("stored initial goal");

        let turn_id = TurnId::new();
        let initial_user_turn = text_user_turn(turn_id, "admitted objective");
        let admission = repository
            .admit_session_turn_with_goal_objective_and_initial_user_turn(
                session_id,
                turn_id,
                "admitted objective",
                Some(&initial_user_turn),
            )
            .await
            .expect("atomic admission")
            .expect("turn admitted");
        let captured = admission.goal.expect("captured goal");
        assert_eq!(captured.goal_id, original_goal_id);
        assert_eq!(captured.goal.objective, "admitted objective");
        assert_eq!(captured.goal.status, ThreadGoalStatus::Active);

        repository
            .replace_thread_goal(
                session_id,
                "replacement after admission",
                ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("replace after admission");
        let (replacement, replacement_goal_id) = repository
            .get_thread_goal_with_id(session_id)
            .await
            .expect("replacement read")
            .expect("replacement goal");
        assert_ne!(replacement_goal_id, captured.goal_id);
        assert_eq!(captured.goal.objective, "admitted objective");

        repository
            .account_thread_goal_usage_for_goal(session_id, 25, Some(captured.goal_id.as_str()))
            .await
            .expect("stale captured usage is ignored");
        let current = repository
            .get_thread_goal(session_id)
            .await
            .expect("current goal")
            .expect("current goal exists");
        assert_eq!(current.objective, replacement.objective);
        assert_eq!(current.tokens_used, 0);
    }

    #[tokio::test]
    async fn active_goal_continuation_admission_is_atomic_and_inactive_is_side_effect_free() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        repository
            .replace_thread_goal(
                session_id,
                "continue until verified",
                ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("active goal");

        let admitted_turn_id = TurnId::new();
        let initial_user_turn = text_user_turn(admitted_turn_id, "continue the goal");
        let admitted = match repository
            .admit_active_goal_continuation_turn_with_initial_user_turn(
                session_id,
                admitted_turn_id,
                Some(&initial_user_turn),
            )
            .await
            .expect("active-goal admission")
        {
            ActiveGoalTurnAdmission::Admitted(snapshot) => snapshot,
            outcome => panic!("active goal was not admitted: {outcome:?}"),
        };
        assert_eq!(
            admitted.goal.as_ref().map(|goal| goal.goal.status),
            Some(ThreadGoalStatus::Active)
        );
        let terminal = completed_terminal(session_id);
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admitted.admission_id,
                    &terminal,
                    admitted_turn_id,
                    None,
                    None,
                )
                .await
                .expect("terminalize continuation"),
            AdmittedTerminalCommit::Applied
        );

        repository
            .update_thread_goal(session_id, None, Some(ThreadGoalStatus::Paused), None)
            .await
            .expect("pause goal")
            .expect("goal retained");
        let before = stored_admission_state(&store, session_id);
        assert!(matches!(
            repository
                .admit_active_goal_continuation_turn(session_id, TurnId::new())
                .await
                .expect("inactive-goal admission"),
            ActiveGoalTurnAdmission::GoalInactive
        ));
        assert_eq!(stored_admission_state(&store, session_id), before);
        assert!(
            !repository
                .has_fresh_run_admission(session_id)
                .await
                .expect("no inactive-goal admission")
        );
    }

    #[tokio::test]
    async fn rejected_goal_change_admission_does_not_mutate_goal() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        repository
            .replace_thread_goal(
                session_id,
                "owned objective",
                ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("initial goal");
        repository
            .admit_session_turn(session_id, TurnId::new())
            .await
            .expect("first admission")
            .expect("first owner");

        assert!(
            repository
                .admit_session_turn_with_goal_objective(
                    session_id,
                    TurnId::new(),
                    "must not be stored",
                )
                .await
                .expect("rejected admission")
                .is_none()
        );
        let goal = repository
            .get_thread_goal(session_id)
            .await
            .expect("goal read")
            .expect("goal retained");
        assert_eq!(goal.objective, "owned objective");
    }

    #[tokio::test]
    async fn expired_owner_is_recovered_before_atomic_replacement_admission() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let admitted_at_ms = SystemClock::now_ms();
        let expired_turn_id = TurnId::new();
        let expired_admission_id = repository
            .admit_session_turn_at(session_id, expired_turn_id, admitted_at_ms, 100)
            .await
            .expect("expired owner setup")
            .expect("expired owner admitted");
        let replacement_turn_id = TurnId::new();
        let replacement_admission_id = repository
            .admit_session_turn_at(
                session_id,
                replacement_turn_id,
                admitted_at_ms.saturating_add(101),
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect("replacement admission")
            .expect("replacement admitted");

        let state = stored_admission_state(&store, session_id);
        assert_eq!(state.0, "running");
        assert_eq!(
            state.1,
            Some(replacement_admission_id.admission_id.to_string())
        );
        assert_eq!(state.2, Some(replacement_turn_id.to_string()));
        assert!(matches!(
            repository
                .renew_admitted_run_lease_at(
                    session_id,
                    expired_admission_id.admission_id,
                    expired_turn_id,
                    admitted_at_ms.saturating_add(102),
                    RUN_ADMISSION_LEASE_DURATION_MS,
                )
                .await
                .expect("stale owner renewal"),
            RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
        ));
        assert_eq!(
            repository
                .durable_terminal_for_turn(session_id, expired_turn_id)
                .await
                .expect("recovery terminal")
                .map(|terminal| terminal.session_status()),
            Some(SessionStatus::Failed)
        );
    }

    #[tokio::test]
    async fn admitted_user_turn_is_the_only_durable_message_contract() {
        let (store, session_id) = test_repo().await;
        let (_, turn_id) = active_turn(&store, session_id).await;
        let history = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history");
        assert!(matches!(
            history.as_slice(),
            [HistoryItem {
                payload: HistoryItemPayload::UserTurn { content, .. },
                ..
            }] if matches!(content.as_slice(), [ContentPart::Text { text }] if text == "canonical request")
        ));
        let repo = store.session_repo();
        let connection = repo.connection.lock().expect("sqlite mutex");
        for retired in ["messages", "message_parts"] {
            let exists = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    params![retired],
                    |row| row.get::<_, bool>(0),
                )
                .expect("schema query");
            assert!(!exists, "retired table {retired} must not exist after V33");
        }
    }

    #[tokio::test]
    async fn pending_tool_sidecar_and_canonical_history_are_one_atomic_bundle() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let repo = store.session_repo();
        repo.connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER abort_tool_sidecar
                 BEFORE INSERT ON tool_calls
                 BEGIN SELECT RAISE(ABORT, 'injected sidecar failure'); END;",
            )
            .expect("trigger");
        let result = repo
            .record_model_response_with_protocol_bundle(
                session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id: ModelResponseId::new(),
                    assistant_text: Some("I will run the command.".to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: vec![PendingToolCallWrite {
                        id: ToolCallId::new(),
                        model_call_id: "model-call-1".to_string(),
                        tool_name: "shell".to_string(),
                        arguments_json: serde_json::json!({"command": "echo ok"}).to_string(),
                        protocol_sequence_no: None,
                    }],
                },
            )
            .await;
        assert!(result.is_err());
        let history = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history");
        assert_eq!(
            history
                .iter()
                .filter(|item| {
                    matches!(
                        item.payload,
                        HistoryItemPayload::AssistantMessage { .. }
                            | HistoryItemPayload::ToolCall { .. }
                    )
                })
                .count(),
            0,
            "failed sidecar insert must roll back the complete model response bundle"
        );
        assert_eq!(
            store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events")
                .iter()
                .filter(|event| matches!(event.msg, RuntimeEventMsg::ToolLifecycle { .. }))
                .count(),
            0,
            "failed sidecar insert must roll back its runtime projection"
        );
    }

    #[tokio::test]
    async fn pending_tool_call_preserves_unknown_name_and_invalid_provider_json_verbatim() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        let raw_tool_name = "provider_tool_not_in_router".to_string();
        let raw_arguments_json = "{not-json}".to_string();
        let events = store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id,
                    assistant_text: None,
                    assistant_protocol_sequence_no: None,
                    tool_calls: vec![PendingToolCallWrite {
                        id: call_id,
                        model_call_id: "provider-call-raw".to_string(),
                        tool_name: raw_tool_name.clone(),
                        arguments_json: raw_arguments_json.clone(),
                        protocol_sequence_no: None,
                    }],
                },
            )
            .await
            .expect("raw pending tool call");

        assert!(matches!(
            events.as_slice(),
            [RunEvent::ToolCallPending {
                tool_call_id: stored_call_id,
                response_id: stored_response_id,
                model_call_id,
                tool_name,
                arguments_json,
            }] if *stored_call_id == call_id
                && *stored_response_id == response_id
                && model_call_id == "provider-call-raw"
                && tool_name == &raw_tool_name
                && arguments_json == &raw_arguments_json
        ));
        let history = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("canonical raw history");
        assert!(history.iter().any(|item| matches!(
            &item.payload,
            HistoryItemPayload::ToolCall {
                call_id: stored_call_id,
                response_id: stored_response_id,
                model_call_id,
                tool_name,
                arguments_json,
            } if *stored_call_id == call_id
                && *stored_response_id == response_id
                && model_call_id == "provider-call-raw"
                && tool_name == &raw_tool_name
                && arguments_json == &raw_arguments_json
        )));
        let sidecar = store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT status, history_item_id FROM tool_calls WHERE id = ?1",
                [call_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("minimal pending sidecar");
        assert_eq!(sidecar.0, "pending");
        assert!(history.iter().any(|item| item.id.to_string() == sidecar.1));
    }

    #[tokio::test]
    async fn complete_model_response_bundle_commits_all_calls_before_execution() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let response_id = ModelResponseId::new();
        let first_call_id = ToolCallId::new();
        let second_call_id = ToolCallId::new();
        let events = store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id,
                    assistant_text: Some("I will inspect both inputs.".to_string()),
                    assistant_protocol_sequence_no: Some(0),
                    tool_calls: vec![
                        PendingToolCallWrite {
                            id: first_call_id,
                            model_call_id: "provider-call-a".to_string(),
                            tool_name: "read".to_string(),
                            arguments_json: serde_json::json!({"path": "a.txt"}).to_string(),
                            protocol_sequence_no: Some(1),
                        },
                        PendingToolCallWrite {
                            id: second_call_id,
                            model_call_id: "provider-call-b".to_string(),
                            tool_name: "read".to_string(),
                            arguments_json: serde_json::json!({"path": "b.txt"}).to_string(),
                            protocol_sequence_no: Some(2),
                        },
                    ],
                },
            )
            .await
            .expect("model response bundle");
        assert_eq!(events.len(), 3);

        let history = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history");
        let response_history = history
            .iter()
            .filter(|item| {
                matches!(
                    item.payload,
                    HistoryItemPayload::AssistantMessage { .. }
                        | HistoryItemPayload::ToolCall { .. }
                )
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            response_history.as_slice(),
            [
                HistoryItem {
                    payload: HistoryItemPayload::AssistantMessage { response_id: stored, .. },
                    ..
                },
                HistoryItem {
                    payload: HistoryItemPayload::ToolCall { call_id: first, response_id: first_response, .. },
                    ..
                },
                HistoryItem {
                    payload: HistoryItemPayload::ToolCall { call_id: second, response_id: second_response, .. },
                    ..
                }
            ] if *stored == response_id
                && *first == first_call_id
                && *second == second_call_id
                && *first_response == response_id
                && *second_response == response_id
        ));
        let sidecar_count = store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("sidecar count");
        assert_eq!(sidecar_count, 2);
    }

    #[tokio::test]
    async fn rollback_is_one_transaction_and_preserves_session_scoped_mode() {
        let (store, session_id) = test_repo().await;
        let protocol = store.protocol_event_store();
        protocol
            .set_collaboration_mode(session_id, ModeKind::Plan)
            .expect("store plan mode")
            .expect("plan instruction");
        let (admission_id, real_turn) = active_turn(&store, session_id).await;
        protocol
            .set_collaboration_mode(session_id, ModeKind::Default)
            .expect("store default mode")
            .expect("default instruction");
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &completed_terminal(session_id),
                    real_turn,
                    None,
                    None,
                )
                .await
                .expect("terminal"),
            AdmittedTerminalCommit::Applied
        );

        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER abort_session_rollback
                 BEFORE UPDATE OF status ON sessions
                 BEGIN SELECT RAISE(ABORT, 'injected rollback reset failure'); END;",
            )
            .expect("rollback failure trigger");
        assert!(
            store
                .session_repo()
                .rollback_session_transaction(session_id, 1)
                .await
                .is_err()
        );
        assert_eq!(
            protocol
                .list_history_items_for_session(session_id)
                .expect("history after failed rollback")
                .len(),
            3,
            "a reset failure must roll turn deletion back while retaining session state"
        );
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch("DROP TRIGGER abort_session_rollback;")
            .expect("drop rollback failure trigger");

        let result = store
            .session_repo()
            .rollback_session_transaction(session_id, 1)
            .await
            .expect("rollback latest turn");
        assert_eq!(result.dropped_turn_ids, vec![real_turn]);
        assert_eq!(result.remaining_history_items, 2);
        assert_eq!(result.session.status, SessionStatus::Idle);
        assert_eq!(
            protocol
                .collaboration_mode_for_session(session_id)
                .expect("mode after rollback"),
            ModeKind::Default
        );
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        for table in [
            "protocol_runtime_events",
            "protocol_history_items",
            "protocol_turn_items",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ] {
            let sql =
                format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1 AND turn_id = ?2");
            let count = connection
                .query_row(
                    &sql,
                    params![session_id.to_string(), real_turn.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("rolled-back table count");
            assert_eq!(count, 0, "{table} retained rolled-back turn state");
        }
    }

    #[tokio::test]
    async fn rollback_rejects_an_active_admission_anywhere_in_the_root_tree() {
        let (store, root_session_id) = test_repo().await;
        let root = store
            .session_repo()
            .get_session(root_session_id)
            .await
            .expect("root session");
        let child = store
            .session_repo()
            .create_session(NewSession {
                project_id: root.project_id,
                title: "child".to_string(),
                cwd: root.cwd.clone(),
                model: root.model.clone(),
                base_url: root.base_url.clone(),
                access_mode: root.access_mode,
            })
            .await
            .expect("child session");
        store
            .session_repo()
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("spawn edge");
        store
            .protocol_event_store()
            .set_collaboration_mode(root_session_id, ModeKind::Plan)
            .expect("root history")
            .expect("root mode item");
        store
            .session_repo()
            .admit_session_turn(child.id, TurnId::new())
            .await
            .expect("child admission")
            .expect("child admitted");

        let error = store
            .session_repo()
            .rollback_session_transaction(root_session_id, 1)
            .await
            .expect_err("active child must block root rollback");
        assert!(error.to_string().contains(&child.id.to_string()));
        assert_eq!(
            store
                .protocol_event_store()
                .list_history_items_for_session(root_session_id)
                .expect("retained root history")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn active_fork_settles_unfinished_calls_before_its_interrupted_terminal() {
        let (store, source_session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, source_session_id).await;
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                source_session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id,
                    assistant_text: Some("I will inspect the file.".to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: vec![PendingToolCallWrite {
                        id: call_id,
                        model_call_id: "provider-call".to_string(),
                        tool_name: "read".to_string(),
                        arguments_json: serde_json::json!({"path": "README.md"}).to_string(),
                        protocol_sequence_no: None,
                    }],
                },
            )
            .await
            .expect("pending response");

        let fork = store
            .session_repo()
            .fork_session_snapshot(source_session_id, Some("snapshot".to_string()))
            .await
            .expect("active snapshot fork");
        assert!(fork.interrupted_live_snapshot);
        let forked_history = store
            .protocol_event_store()
            .list_history_items(fork.forked_session.id, turn_id)
            .expect("forked history");
        assert!(forked_history.iter().any(|item| matches!(
            item.payload,
            HistoryItemPayload::ToolOutput {
                call_id: stored_call_id,
                status: ToolLifecycleStatus::Cancelled,
                ..
            } if stored_call_id == call_id
        )));
        let terminal = store
            .session_repo()
            .durable_terminal_for_turn(fork.forked_session.id, turn_id)
            .await
            .expect("fork terminal read")
            .expect("fork terminal");
        assert!(matches!(
            terminal.outcome,
            TurnTerminalOutcome::Interrupted { .. }
        ));
        assert_eq!(terminal.final_response_id, None);
        assert_eq!(terminal.tool_call_count, 1);
        assert_eq!(terminal.failed_tool_count, 0);
        assert_eq!(terminal.change_count, 0);
        let forked_turn_items = store
            .protocol_event_store()
            .list_turn_items(fork.forked_session.id, turn_id)
            .expect("forked turn items");
        let cancelled_position = forked_turn_items
            .iter()
            .position(|item| {
                matches!(
                    item.payload,
                    TurnItemPayload::ToolStatus {
                        call_id: stored_call_id,
                        status: ToolLifecycleStatus::Cancelled,
                        ..
                    } if stored_call_id == call_id
                )
            })
            .expect("cancelled projection");
        let terminal_position = forked_turn_items
            .iter()
            .position(|item| matches!(item.payload, TurnItemPayload::Terminal { .. }))
            .expect("terminal projection");
        assert!(cancelled_position < terminal_position);
    }

    #[tokio::test]
    async fn active_fork_rejects_a_source_without_an_active_turn() {
        let (store, source_session_id) = test_repo().await;
        store
            .session_repo()
            .inject_raw_runtime_state_for_corruption_test(
                source_session_id,
                "running",
                None,
                None,
                None,
            )
            .expect("create impossible running source fixture");

        let error = store
            .session_repo()
            .fork_session_snapshot(source_session_id, Some("invalid snapshot".to_string()))
            .await
            .expect_err("fork must fail closed without an active turn");

        assert!(error.to_string().contains("durable run admission"));
    }

    #[tokio::test]
    async fn expired_admission_recovery_derives_terminal_after_user_turn_crash() {
        let (store, session_id) = test_repo().await;
        let (_, turn_id) = active_turn(&store, session_id).await;
        expire_and_recover_run(&store, session_id).await;

        let terminal = store
            .session_repo()
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("terminal read")
            .expect("recovery terminal");
        assert_eq!(terminal.final_response_id, None);
        assert_eq!(terminal.tool_call_count, 0);
        assert_eq!(terminal.failed_tool_count, 0);
        assert_eq!(terminal.change_count, 0);
    }

    #[tokio::test]
    async fn expired_admission_recovery_derives_response_and_failed_pending_call() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id,
                    assistant_text: Some("Calling the tool.".to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: vec![PendingToolCallWrite {
                        id: call_id,
                        model_call_id: "provider-call".to_string(),
                        tool_name: "read".to_string(),
                        arguments_json: serde_json::json!({"path": "README.md"}).to_string(),
                        protocol_sequence_no: None,
                    }],
                },
            )
            .await
            .expect("model response");
        expire_and_recover_run(&store, session_id).await;

        let terminal = store
            .session_repo()
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("terminal read")
            .expect("recovery terminal");
        assert_eq!(terminal.final_response_id, None);
        assert_eq!(terminal.tool_call_count, 1);
        assert_eq!(terminal.failed_tool_count, 1);
        assert_eq!(terminal.change_count, 0);
        assert!(
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .expect("recovered history")
                .iter()
                .any(|item| matches!(
                    item.payload,
                    HistoryItemPayload::ToolOutput {
                        call_id: stored_call_id,
                        status: ToolLifecycleStatus::Failed,
                        ..
                    } if stored_call_id == call_id
                ))
        );
    }

    #[tokio::test]
    async fn expired_admission_recovery_derives_completed_tool_and_change_counts() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                session_id,
                admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id,
                    assistant_text: None,
                    assistant_protocol_sequence_no: None,
                    tool_calls: vec![PendingToolCallWrite {
                        id: call_id,
                        model_call_id: "provider-call".to_string(),
                        tool_name: "apply_patch".to_string(),
                        arguments_json: serde_json::json!({"patch": "test"}).to_string(),
                        protocol_sequence_no: None,
                    }],
                },
            )
            .await
            .expect("model response");
        let durable_changes = vec![
            crate::edit::FileChange {
                id: ChangeId::new(),
                tool_call_id: call_id,
                kind: ChangeKind::Update,
                path_before: Some("a.txt".into()),
                path_after: Some("a.txt".into()),
                before_sha256: Some("before-a".to_string()),
                after_sha256: Some("after-a".to_string()),
                diff_text: "a changed".to_string(),
                summary: "updated a.txt".to_string(),
                created_at_ms: 1,
            },
            crate::edit::FileChange {
                id: ChangeId::new(),
                tool_call_id: call_id,
                kind: ChangeKind::Add,
                path_before: None,
                path_after: Some("b.txt".into()),
                before_sha256: None,
                after_sha256: Some("after-b".to_string()),
                diff_text: "b added".to_string(),
                summary: "added b.txt".to_string(),
                created_at_ms: 1,
            },
        ];
        store
            .change_repo()
            .insert_changes(&durable_changes)
            .await
            .expect("durable file-change evidence");
        let changes = durable_changes
            .iter()
            .map(|change| crate::edit::ChangeSummary {
                change_id: change.id,
                kind: change.kind,
                path_before: change.path_before.clone(),
                path_after: change.path_after.clone(),
            })
            .collect();
        store
            .session_repo()
            .complete_tool_call_with_file_changes_protocol_bundle(
                session_id,
                admission_id,
                call_id,
                crate::tool::ToolName::ApplyPatch,
                "apply_patch",
                serde_json::json!({"success": true}),
                "updated files",
                None,
                changes,
                turn_id,
                None,
                None,
            )
            .await
            .expect("tool settlement")
            .expect("tool settled with canonical changes");
        expire_and_recover_run(&store, session_id).await;

        let terminal = store
            .session_repo()
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("terminal read")
            .expect("recovery terminal");
        assert_eq!(terminal.final_response_id, None);
        assert_eq!(terminal.tool_call_count, 1);
        assert_eq!(terminal.failed_tool_count, 0);
        assert_eq!(terminal.change_count, 2);
    }

    #[tokio::test]
    async fn current_turn_terminal_finish_drains_pending_mail_without_resampling() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let stored_active_mail = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "new evidence".to_string(),
                    trigger_turn: false,
                },
                true,
            )
            .expect("active mail append");
        assert!(!stored_active_mail.schedule_turn);
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(session_id)
                .expect("history before delivery")
                .iter()
                .all(|item| item.id != stored_active_mail.history_item_id),
            "enqueue must not write model-visible history"
        );
        let terminal = completed_terminal(session_id);
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &terminal,
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("terminal CAS with finish-drain"),
            AdmittedTerminalCommit::Applied
        );
        let active_mail = store
            .protocol_event_store()
            .history_items_by_id(session_id, &[stored_active_mail.history_item_id])
            .expect("delivered history")
            .into_iter()
            .next()
            .expect("active mail history");
        assert_eq!(
            active_mail.scope,
            crate::protocol::HistoryScope::Turn { turn_id }
        );
        let append_after_terminal = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "too late".to_string(),
                    trigger_turn: false,
                },
                true,
            );
        assert!(append_after_terminal.is_err());
        assert_eq!(
            store
                .protocol_event_store()
                .list_history_items_for_session(session_id)
                .expect("history")
                .iter()
                .filter(|item| matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn failed_terminal_finish_drains_eligible_pending_mail_without_resampling() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let queued = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "evidence racing a failed terminal".to_string(),
                    trigger_turn: false,
                },
                true,
            )
            .expect("eligible pending mail");

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &failed_terminal(session_id, "provider failed"),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("failed terminal with finish-drain"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .protocol_event_store()
                .history_items_by_id(session_id, &[queued.history_item_id])
                .expect("finish-drained failure history")
                .len(),
            1
        );
        assert!(
            !store
                .session_repo()
                .has_pending_agent_mailbox_messages(session_id)
                .expect("mailbox after failure finish-drain")
        );
    }

    #[tokio::test]
    async fn inactive_recipient_mail_stays_out_of_history_until_a_safe_turn_delivery() {
        let (store, session_id) = test_repo().await;
        let (admission_id, completed_turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &completed_terminal(session_id),
                    completed_turn_id,
                    None,
                    None,
                )
                .await
                .expect("terminal"),
            AdmittedTerminalCommit::Applied
        );

        let communication_id = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "evidence for a future continuation".to_string(),
                    trigger_turn: false,
                },
                false,
            )
            .expect("inactive recipient communication");
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(session_id)
                .expect("history before continuation")
                .iter()
                .all(|item| item.id != communication_id.history_item_id)
        );
        let queued = store
            .session_repo()
            .agent_mailbox_communications_by_id(session_id, &[communication_id.history_item_id])
            .expect("durable pending mailbox");
        assert_eq!(queued.len(), 1);

        let continuation_turn_id = TurnId::new();
        let continuation = store
            .session_repo()
            .admit_session_turn(session_id, continuation_turn_id)
            .await
            .expect("continuation admission")
            .expect("idle session admitted");
        let delivered = store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                session_id,
                continuation.admission_id,
                continuation_turn_id,
                128,
            )
            .expect("deliver queued evidence");
        assert_eq!(
            delivered.history_item_ids,
            vec![communication_id.history_item_id]
        );
        let communication = store
            .protocol_event_store()
            .history_items_by_id(session_id, &[communication_id.history_item_id])
            .expect("delivered history")
            .into_iter()
            .next()
            .expect("communication item");

        assert_eq!(
            communication.scope,
            crate::protocol::HistoryScope::Turn {
                turn_id: continuation_turn_id
            }
        );
        assert!(matches!(
            communication.payload,
            HistoryItemPayload::InterAgentCommunication { .. }
        ));
        assert!(
            store
                .session_repo()
                .durable_terminal_for_turn(session_id, completed_turn_id)
                .await
                .expect("terminal read")
                .is_some()
        );
    }

    #[tokio::test]
    async fn running_recipient_followup_survives_interruption_until_next_safe_delivery() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (admission_id, turn_id) = active_turn(&store, child.id).await;
        let repository = store.session_repo();
        let followup = repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/child",
                        "/root",
                        "follow up before the current turn reaches a safe mailbox boundary",
                    ),
                    trigger_turn: true,
                },
                true,
            )
            .expect("running-recipient follow-up");
        assert!(!followup.schedule_turn);
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    admission_id,
                    &agent_interrupted_terminal(child.id),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("immediate child interruption"),
            AdmittedTerminalCommit::Applied
        );

        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending follow-up after interruption"),
            Some(followup.history_item_id)
        );
        let continuation_turn_id = TurnId::new();
        let continuation = repository
            .admit_agent_triggered_turn(child.id, continuation_turn_id, followup.history_item_id)
            .await
            .expect("recoverable follow-up admission")
            .expect("follow-up admitted");
        let delivered = repository
            .deliver_pending_agent_mail_for_admitted_turn(
                child.id,
                continuation.admission_id,
                continuation_turn_id,
                128,
            )
            .expect("follow-up delivery");
        assert_eq!(delivered.history_item_ids, vec![followup.history_item_id]);
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger after delivery"),
            None
        );
        let history = store
            .protocol_event_store()
            .history_items_by_id(child.id, &[followup.history_item_id])
            .expect("delivered follow-up history");
        assert!(matches!(
            history.as_slice(),
            [item]
                if item.scope
                    == (HistoryScope::Turn {
                        turn_id: continuation_turn_id,
                    })
        ));
    }

    #[tokio::test]
    async fn next_turn_phase_allows_ordinary_mail_to_remain_pending_after_success() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let queued = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "arrived after the final sample".to_string(),
                    trigger_turn: false,
                },
                true,
            )
            .expect("ordinary pending mail");

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                    session_id,
                    admission_id,
                    &completed_terminal(session_id),
                    turn_id,
                    None,
                    false,
                    None,
                )
                .await
                .expect("NextTurn terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            store
                .session_repo()
                .has_pending_agent_mailbox_messages(session_id)
                .expect("pending mailbox")
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(session_id)
                .expect("history")
                .iter()
                .all(|item| item.id != queued.history_item_id)
        );
    }

    #[tokio::test]
    async fn next_turn_phase_leaves_direct_child_final_pending_without_blocking_terminal() {
        let (store, root_session_id) = test_repo().await;
        let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, None),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("child terminal"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("handoff query")
            .expect("pending child FINAL");

        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                    root_session_id,
                    root_admission_id,
                    &completed_terminal(root_session_id),
                    root_turn_id,
                    None,
                    false,
                    None,
                )
                .await
                .expect("root NextTurn terminal"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            store
                .session_repo()
                .has_pending_agent_mailbox_messages(root_session_id)
                .expect("pending FINAL")
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items(root_session_id, root_turn_id)
                .expect("terminal turn history")
                .iter()
                .all(|item| item.id != handoff.history_item_id)
        );
        assert_eq!(
            store
                .session_repo()
                .agent_mailbox_communications_by_id(root_session_id, &[handoff.history_item_id],)
                .expect("pending FINAL")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn required_only_delivery_keeps_late_ordinary_mail_pending_when_forced_sample_is_final() {
        let (store, root_session_id) = test_repo().await;
        let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let ordinary = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                root_session_id,
                InterAgentCommunication {
                    author: "/root/child".to_string(),
                    recipient: "/root".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::Message,
                        "/root",
                        "/root/child",
                        "ordinary mail after the owner's visible final",
                    ),
                    trigger_turn: false,
                },
                true,
            )
            .expect("late ordinary mail");
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, None),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("child terminal"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("handoff query")
            .expect("required child FINAL");

        let required = store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn_with_selector(
                root_session_id,
                root_admission_id,
                root_turn_id,
                AgentMailboxDeliverySelector::RequiredChildResultsOnly,
                128,
            )
            .expect("required-only safe boundary");
        assert_eq!(required.history_item_ids, vec![handoff.history_item_id]);
        assert!(!required.has_more);
        assert_eq!(
            store
                .session_repo()
                .agent_mailbox_communications_by_id(root_session_id, &[ordinary.history_item_id],)
                .expect("ordinary mail remains queued")
                .len(),
            1
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(root_session_id)
                .expect("owner history")
                .iter()
                .all(|item| item.id != ordinary.history_item_id)
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                    root_session_id,
                    root_admission_id,
                    &completed_terminal_for_response(root_session_id, None),
                    root_turn_id,
                    None,
                    false,
                    None,
                )
                .await
                .expect("terminal after required result"),
            AdmittedTerminalCommit::Applied
        );
        assert!(
            store
                .session_repo()
                .has_pending_agent_mailbox_messages(root_session_id)
                .expect("next-turn ordinary mail")
        );
    }

    #[tokio::test]
    async fn current_turn_all_after_required_tool_call_delivers_late_ordinary_mail() {
        let (store, root_session_id) = test_repo().await;
        let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
        let ordinary = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                root_session_id,
                InterAgentCommunication {
                    author: "/root/child".to_string(),
                    recipient: "/root".to_string(),
                    content: "ordinary mail queued beside required FINAL".to_string(),
                    trigger_turn: false,
                },
                true,
            )
            .expect("late ordinary mail");
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    child_admission_id,
                    &completed_terminal_for_response(child.id, None),
                    child_turn_id,
                    None,
                    None,
                )
                .await
                .expect("child terminal"),
            AdmittedTerminalCommit::Applied
        );
        let handoff = store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("handoff query")
            .expect("required child FINAL");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn_with_selector(
                    root_session_id,
                    root_admission_id,
                    root_turn_id,
                    AgentMailboxDeliverySelector::RequiredChildResultsOnly,
                    128,
                )
                .expect("host-forced required sample")
                .history_item_ids,
            vec![handoff.history_item_id]
        );

        // The forced sample emitted a tool call. Codex reopens the next safe
        // boundary to every current-turn mailbox item.
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn_with_selector(
                    root_session_id,
                    root_admission_id,
                    root_turn_id,
                    AgentMailboxDeliverySelector::AllPending,
                    128,
                )
                .expect("safe boundary after required-sample tool call")
                .history_item_ids,
            vec![ordinary.history_item_id]
        );
        assert!(
            !store
                .session_repo()
                .has_pending_agent_mailbox_messages(root_session_id)
                .expect("mailbox after tool boundary")
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(root_session_id)
                .expect("owner history")
                .iter()
                .any(|item| item.id == ordinary.history_item_id)
        );
    }

    #[tokio::test]
    async fn delivered_followup_is_not_retriggered_after_interrupt_or_restart() {
        let (store, root_session_id) = test_repo().await;
        let child =
            retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
        let (admission_id, turn_id) = active_turn(&store, child.id).await;
        let followup = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/child",
                        "/root",
                        "deliver before interruption",
                    ),
                    trigger_turn: true,
                },
                true,
            )
            .expect("running follow-up");
        let delivered = store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(child.id, admission_id, turn_id, 128)
            .expect("safe delivery");
        assert_eq!(delivered.history_item_ids, vec![followup.history_item_id]);
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    admission_id,
                    &agent_interrupted_terminal(child.id),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("interrupt after delivery"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("pending trigger"),
            None
        );
        assert_eq!(
            store
                .session_repo()
                .admit_agent_triggered_turn(child.id, TurnId::new(), followup.history_item_id)
                .await
                .expect("delivered trigger admission"),
            None
        );

        let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen store");
        reopened_sqlite.migrate().expect("validate reopened store");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("reopened pending trigger"),
            None
        );
        assert_eq!(
            reopened
                .protocol_event_store()
                .list_history_items_for_session(child.id)
                .expect("reopened child history")
                .iter()
                .filter(|item| item.id == followup.history_item_id)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn rollback_targets_only_real_turns_and_preserves_all_idle_mail() {
        let (store, session_id) = test_repo().await;
        let (admission_id, completed_turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &completed_terminal(session_id),
                    completed_turn_id,
                    None,
                    None,
                )
                .await
                .expect("terminal"),
            AdmittedTerminalCommit::Applied
        );
        let first_mail_id = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "first future evidence".to_string(),
                    trigger_turn: false,
                },
                false,
            )
            .expect("first inactive mail");
        let second_mail_id = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "second future evidence".to_string(),
                    trigger_turn: false,
                },
                false,
            )
            .expect("second inactive mail");
        let rolled_back = store
            .session_repo()
            .rollback_session_transaction(session_id, 1)
            .await
            .expect("rollback latest real turn");
        assert_eq!(rolled_back.dropped_turn_ids, vec![completed_turn_id]);
        let queued = store
            .session_repo()
            .agent_mailbox_communications_by_id(
                session_id,
                &[
                    first_mail_id.history_item_id,
                    second_mail_id.history_item_id,
                ],
            )
            .expect("mailbox after rollback");
        assert_eq!(queued.len(), 2);
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(session_id)
                .expect("history after rollback")
                .iter()
                .all(|item| item.id != first_mail_id.history_item_id
                    && item.id != second_mail_id.history_item_id)
        );
        assert!(
            store
                .session_repo()
                .durable_terminal_for_turn(session_id, completed_turn_id)
                .await
                .expect("completed terminal read")
                .is_none(),
            "rollback must remove the selected real turn without consuming session mail"
        );
    }

    #[tokio::test]
    async fn rollback_rejects_turn_owned_by_terminalized_owner_resume_request() {
        for tree_stopped in [false, true] {
            let (store, root_session_id) = test_repo().await;
            let repository = store.session_repo();
            let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
            let request_id = requests[0].request_id;
            let owner_turn = TurnId::new();
            let owner_admission = repository
                .admit_owner_resume_turn(owner.id, owner_turn, request_id)
                .await
                .expect("OwnerResume admission")
                .expect("OwnerResume admitted");
            if tree_stopped {
                repository
                    .record_agent_tree_stop_fence(
                        root_session_id,
                        crate::protocol::TurnInterruptionCause::UserStop,
                    )
                    .await
                    .expect("record ancestor OwnerResume tree-stop fence")
                    .expect("ancestor OwnerResume tree-stop fence");
            }
            let terminal = if tree_stopped {
                tree_stopped_terminal(owner.id)
            } else {
                agent_interrupted_terminal(owner.id)
            };
            assert_eq!(
                repository
                    .terminalize_admitted_turn_with_protocol_event(
                        owner.id,
                        owner_admission.admission_id,
                        &terminal,
                        owner_turn,
                        None,
                        None,
                    )
                    .await
                    .expect("owner interruption"),
                AdmittedTerminalCommit::Applied
            );

            let error = repository
                .rollback_session_transaction(owner.id, 1)
                .await
                .expect_err("OwnerResume-owned turn rollback must be rejected");
            assert!(
                error
                    .to_string()
                    .contains("durable OwnerResume request claim"),
                "unexpected rollback rejection: {error}"
            );
            assert!(
                !store
                    .protocol_event_store()
                    .list_runtime_events(owner.id, owner_turn)
                    .expect("retained OwnerResume turn")
                    .is_empty()
            );
            let (state, claimed_turn_id) = repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .query_row(
                    "SELECT state, claimed_turn_id
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                    params![owner.id.to_string(), request_id.to_string()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .expect("retained OwnerResume row");
            assert!(
                matches!(state.as_str(), "resolved" | "cancelled"),
                "unexpected retained OwnerResume state: {state}"
            );
            assert_eq!(claimed_turn_id, Some(owner_turn.to_string()));
        }
    }

    #[tokio::test]
    async fn rollback_rejects_a_turn_owned_by_an_explicit_wake_claim() {
        let (store, root_session_id) = test_repo().await;
        let (child, history_item_id, _) =
            spawn_pending_child(&store, root_session_id, "rollback_claim").await;
        let repository = store.session_repo();
        let turn_id = TurnId::new();
        repository
            .admit_agent_triggered_turn(child.id, turn_id, history_item_id)
            .await
            .expect("explicit admission")
            .expect("explicit wake admitted");
        assert!(matches!(
            repository
                .settle_agent_execution_wake_with_terminal(
                    child.id,
                    AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id),
                    pre_admission_agent_interrupted_terminal(),
                )
                .expect("exact wake settlement"),
            AgentExecutionWakeTerminalSettlement::Applied {
                turn_id: settled_turn_id,
                ..
            } if settled_turn_id == turn_id
        ));
        let root_admission_id = repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT active_run_id FROM sessions WHERE id = ?1",
                [root_session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .expect("root admission id")
            .parse::<AdmissionId>()
            .expect("valid root admission id");
        let root_target = repository
            .captured_running_terminal_target(root_session_id)
            .await
            .expect("capture root target")
            .expect("root remains admitted");
        assert!(
            repository
                .terminalize_captured_running_session_with_protocol_event(
                    root_session_id,
                    &completed_terminal_for_response(root_session_id, None),
                    root_target,
                )
                .await
                .expect("complete detached root")
        );
        assert!(
            repository
                .release_stopped_run_admission(root_session_id, root_admission_id)
                .await
                .expect("release retained root admission")
        );

        let error = repository
            .rollback_session_transaction(child.id, 1)
            .await
            .expect_err("explicit wake identity must survive rollback");
        assert!(
            error.to_string().contains("explicit agent wake claim"),
            "unexpected rollback rejection: {error}"
        );
        assert!(
            repository
                .durable_terminal_for_turn(child.id, turn_id)
                .await
                .expect("retained explicit terminal")
                .is_some()
        );
    }

    #[tokio::test]
    async fn rollback_rejects_a_turn_containing_delivered_mailbox_input() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let stored = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root".to_string(),
                    content: "immutable delivered input".to_string(),
                    trigger_turn: false,
                },
                true,
            )
            .expect("pending self mail");
        assert_eq!(
            store
                .session_repo()
                .deliver_pending_agent_mail_for_admitted_turn(
                    session_id,
                    admission_id,
                    turn_id,
                    128,
                )
                .expect("deliver self mail")
                .history_item_ids,
            vec![stored.history_item_id]
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &agent_interrupted_terminal(session_id),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("close delivered turn"),
            AdmittedTerminalCommit::Applied
        );

        let error = store
            .session_repo()
            .rollback_session_transaction(session_id, 1)
            .await
            .expect_err("delivered mailbox lifecycle must block rollback");
        assert!(error.to_string().contains("mailbox delivery is immutable"));
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT state
                     FROM agent_mailbox_messages
                     WHERE id = ?1",
                    params![stored.history_item_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .expect("mailbox state after rejected rollback"),
            "delivered"
        );
        assert!(
            connection
                .query_row(
                    "SELECT EXISTS(
                     SELECT 1
                     FROM protocol_history_items
                     WHERE id = ?1 AND turn_id = ?2
                 )",
                    params![stored.history_item_id.to_string(), turn_id.to_string()],
                    |row| row.get::<_, bool>(0),
                )
                .expect("history after rejected rollback")
        );
    }

    #[tokio::test]
    async fn admitted_terminal_is_first_writer_and_is_rehydrated_as_one_typed_value() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let repo = store.session_repo();
        let event = completed_terminal(session_id);
        assert_eq!(
            repo.terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &event,
                turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize"),
            AdmittedTerminalCommit::Applied
        );
        let durable = repo
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("read terminal")
            .expect("terminal");
        assert!(matches!(durable.outcome, TurnTerminalOutcome::Completed));
        assert_eq!(durable.summary(), "completed");
        assert_eq!(
            store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("events")
                .iter()
                .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
                .count(),
            1
        );
        assert_eq!(
            repo.terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &completed_terminal(session_id),
                turn_id,
                None,
                None,
            )
            .await
            .expect("second terminal attempt"),
            AdmittedTerminalCommit::NotOwned
        );
        assert_eq!(
            repo.durable_terminal_for_turn(session_id, turn_id)
                .await
                .expect("read terminal")
                .expect("terminal")
                .summary(),
            "completed"
        );
    }

    #[tokio::test]
    async fn captured_terminal_target_cannot_terminalize_a_replacement_turn() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let (first_admission_id, first_turn_id) = active_turn(&store, session_id).await;
        let first_target = repository
            .captured_running_terminal_target(session_id)
            .await
            .expect("capture first target")
            .expect("first running target");
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    first_admission_id,
                    &completed_terminal(session_id),
                    first_turn_id,
                    None,
                    None,
                )
                .await
                .expect("complete first turn"),
            AdmittedTerminalCommit::Applied
        );

        let second_turn_id = TurnId::new();
        repository
            .admit_session_turn(session_id, second_turn_id)
            .await
            .expect("replacement admission")
            .expect("replacement admitted");
        assert!(
            !repository
                .terminalize_captured_running_session_with_protocol_event(
                    session_id,
                    &failed_terminal(session_id, "stale stop target"),
                    first_target,
                )
                .await
                .expect("stale target is a clean CAS miss")
        );
        assert_eq!(
            repository
                .fresh_running_turn_for_session(session_id)
                .await
                .expect("replacement turn"),
            Some(second_turn_id)
        );
        assert!(
            repository
                .durable_terminal_for_turn(session_id, second_turn_id)
                .await
                .expect("replacement terminal lookup")
                .is_none()
        );
    }

    #[tokio::test]
    async fn captured_terminal_target_survives_same_owner_lease_renewal() {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let target = repository
            .captured_running_terminal_target(session_id)
            .await
            .expect("capture target")
            .expect("running target");
        assert!(matches!(
            repository
                .renew_admitted_run_lease_at(
                    session_id,
                    admission_id,
                    turn_id,
                    SystemClock::now_ms(),
                    RUN_ADMISSION_LEASE_DURATION_MS * 4,
                )
                .await
                .expect("renew same owner"),
            RunAdmissionLeaseRenewalOutcome::Renewed
        ));
        assert!(
            repository
                .terminalize_captured_running_session_with_protocol_event(
                    session_id,
                    &failed_terminal(session_id, "stop after renewal"),
                    target,
                )
                .await
                .expect("terminalize renewed owner")
        );
        assert!(
            repository
                .durable_terminal_for_turn(session_id, turn_id)
                .await
                .expect("terminal lookup")
                .is_some()
        );
    }

    #[tokio::test]
    async fn terminal_recovery_requires_the_captured_matching_owner() {
        let (store, null_turn_session_id) = test_repo().await;
        let repository = store.session_repo();
        repository
            .inject_raw_runtime_state_for_corruption_test(
                null_turn_session_id,
                "running",
                None,
                None,
                None,
            )
            .expect("create turnless running fixture");
        let recovered_turn_id = TurnId::new();
        let error = repository
            .captured_running_terminal_target(null_turn_session_id)
            .await
            .expect_err("turnless recovery must fail closed");
        assert!(error.to_string().contains("durable run admission"));
        assert!(
            repository.get_session(null_turn_session_id).await.is_err(),
            "ordinary reads must reject the same invalid owner state"
        );
        assert!(
            repository
                .durable_terminal_for_turn(null_turn_session_id, recovered_turn_id)
                .await
                .expect("turnless terminal lookup")
                .is_none()
        );

        let (store, owned_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (_admission_id, active_turn_id) = active_turn(&store, owned_session_id).await;
        let owned_target = repository
            .captured_running_terminal_target(owned_session_id)
            .await
            .expect("capture owned target")
            .expect("owned running target");
        let foreign_session =
            create_sibling_session(&store, owned_session_id, "foreign owner").await;
        let (_foreign_admission_id, foreign_turn_id) =
            active_turn(&store, foreign_session.id).await;
        let foreign_target = repository
            .captured_running_terminal_target(foreign_session.id)
            .await
            .expect("capture foreign target")
            .expect("foreign running target");
        assert_ne!(foreign_turn_id, active_turn_id);
        assert!(
            !repository
                .terminalize_captured_running_session_with_protocol_event(
                    owned_session_id,
                    &failed_terminal(owned_session_id, "foreign turn"),
                    foreign_target,
                )
                .await
                .expect("reject foreign recovery turn")
        );
        assert_eq!(
            repository
                .get_session(owned_session_id)
                .await
                .expect("owned session")
                .status,
            SessionStatus::Running
        );
        assert!(
            repository
                .durable_terminal_for_turn(owned_session_id, foreign_turn_id)
                .await
                .expect("foreign terminal lookup")
                .is_none()
        );
        assert!(
            repository
                .recover_captured_running_session_with_protocol_event(
                    owned_session_id,
                    &failed_terminal(owned_session_id, "orphaned run"),
                    owned_target,
                )
                .await
                .expect("recover the exact durable active turn")
        );
        assert_eq!(
            repository
                .get_session(owned_session_id)
                .await
                .expect("recovered session")
                .status,
            SessionStatus::Failed
        );
        assert!(
            repository
                .durable_terminal_for_turn(owned_session_id, active_turn_id)
                .await
                .expect("active terminal lookup")
                .is_some()
        );
    }

    #[test]
    fn terminal_writer_rejects_non_terminal_events_and_invalid_counts() {
        let session_id = SessionId::new();
        let non_terminal = RunEvent::SessionStarted {
            session_id,
            title: "test".to_string(),
        };
        assert!(validate_terminal_event(session_id, &non_terminal).is_err());
        let invalid_counts = RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 1,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert!(validate_terminal_event(session_id, &invalid_counts).is_err());
    }
}
