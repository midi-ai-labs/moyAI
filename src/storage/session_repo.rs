use std::collections::{BTreeMap, HashMap, HashSet};
mod agent_lifecycle;
mod agent_mailbox;
mod history_mutation;
mod origin_guard;
mod projection;
mod run_lease;
mod security_review;
mod session_settings;
mod shared_archive;
mod shared_runs;
mod spawn_edges;
mod thread_goals;
mod tool_calls;
mod turn_steers;
mod turn_terminal;
#[cfg(test)]
use agent_lifecycle::insert_deferred_agent_completion_in_transaction;
use agent_lifecycle::{
    TerminalHandoff, agent_trigger_history_item_for_turn_in_connection,
    agent_trigger_turn_claim_in_connection, agent_work_admissible,
    claim_agent_work_for_admitted_turn, deferred_agent_completion_in_connection,
    discard_pending_crash_deferred_completion_in_transaction,
    discard_pending_deferred_completions_for_fenced_terminal_in_transaction,
    discard_pending_explicit_agent_wake_in_transaction,
    has_pending_owner_resume_requests_in_connection, owner_resume_claimed_turn_in_connection,
    pending_direct_child_result_terminal_in_connection, publish_or_defer_terminal_handoff,
    release_quiescent_deferred_completions_after_interruption_in_transaction,
    repend_claimed_owner_resume_requests_in_transaction,
    resolve_claimed_owner_resume_requests_in_transaction, retained_agent_parent_in_connection,
    settle_pending_agent_trigger_in_transaction, settle_pending_owner_resume_in_transaction,
    supersede_pending_deferred_completion_in_transaction, truncate_agent_completion_middle,
};
use agent_mailbox::{
    deliver_all_pending_agent_mail_in_transaction,
    deliver_claimed_explicit_agent_wake_in_transaction,
    insert_agent_mailbox_message_in_transaction,
};
pub(crate) use history_mutation::delete_session_rows;
use origin_guard::{
    origin_history_revisions_for_tree_in_connection, verify_origin_history_revisions,
};
pub(crate) use security_review::{
    MAX_SECURITY_REVIEW_CHECK_REFERENCE_BYTES, MAX_SECURITY_REVIEW_CHECKS, SecurityReviewCheck,
    SecurityReviewEvidence, SecurityReviewState,
};
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use thread_goals::{
    set_thread_goal_objective_in_transaction, stored_thread_goal_from_connection,
    validate_goal_objective_and_budget,
};
use tool_calls::validate_canonical_tool_call_in_transaction;
use turn_steers::{
    deliver_all_pending_turn_steers_in_transaction, discard_all_pending_turn_steers_in_transaction,
    pending_turn_input_projections_in_transaction,
};

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
    ActiveTurnExpectation, AdmissionId, DurableTurnTerminal, NewSession, ProjectId, RunEvent,
    SessionEditForkResult, SessionForkResult, SessionId, SessionModelParameters,
    SessionProviderConnection, SessionRecord, SessionRepository, SessionSettingsPatch,
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
pub(crate) enum ExactRootExecutionValidationState {
    Running,
    Terminal,
    TargetChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactExecutionInterruptRequestSettlement {
    Recorded,
    AlreadyRequested,
    TargetChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurableExactExecutionInterruptRequest {
    admission_id: AdmissionId,
    turn_id: TurnId,
    admission_revision: u64,
    cause: crate::protocol::TurnInterruptionCause,
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

#[derive(Debug, Clone, Copy)]
enum TerminalOwnerGuard<'a> {
    Admitted {
        admission_id: AdmissionId,
        turn_id: TurnId,
    },
    Captured(RunningSessionTerminalTarget),
    AgentWake(AgentExecutionWakeTerminalOwner),
    SharedCheckpoint(&'a crate::agent::shared::SharedCheckpoint),
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
    fn admitted_settlement(self) -> Result<AdmittedTerminalSettlement, StorageError> {
        match self {
            Self::Settled {
                commit: AdmittedTerminalCommit::Applied,
                terminal,
                ..
            } => Ok(AdmittedTerminalSettlement::Applied { terminal }),
            Self::Settled {
                commit: AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission,
                terminal,
                ..
            } => Ok(AdmittedTerminalSettlement::AlreadyTerminalizedBySameAdmission { terminal }),
            Self::Settled {
                commit: AdmittedTerminalCommit::NotOwned,
                ..
            }
            | Self::NotOwned => Ok(AdmittedTerminalSettlement::NotOwned),
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
    pub admission_revision: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalSessionStorageSnapshot {
    pub session: SessionRecord,
    pub protocol: CanonicalProtocolSnapshot,
    pub active_turn_position: Option<(TurnId, i64)>,
    pub pending_turn_inputs: Vec<crate::session::PendingTurnInputProjection>,
    pub admission_revision: u64,
    pub turn_progress: Option<crate::session::model::CanonicalTurnProgress>,
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
    pub admission_revision: u64,
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
    expected_latest_turn_id: Option<Option<TurnId>>,
    expected_admission_revision: Option<u64>,
    root_continuation_predecessor_turn_id: Option<TurnId>,
    expected_remote_job_id: Option<ulid::Ulid>,
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
            expected_latest_turn_id: None,
            expected_admission_revision: None,
            root_continuation_predecessor_turn_id: None,
            expected_remote_job_id: None,
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
            expected_latest_turn_id: None,
            expected_admission_revision: None,
            root_continuation_predecessor_turn_id: None,
            expected_remote_job_id: None,
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
            expected_latest_turn_id: None,
            expected_admission_revision: None,
            root_continuation_predecessor_turn_id: None,
            expected_remote_job_id: None,
        }
    }

    fn root_continuation_after_turn(
        turn_id: TurnId,
        initial_user_turn: Option<&UserTurn>,
        expected_predecessor_turn_id: TurnId,
        expected_predecessor_revision: u64,
        require_active_goal: bool,
    ) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::Preserve,
            goal_requirement: if require_active_goal {
                TurnGoalAdmissionRequirement::Active
            } else {
                TurnGoalAdmissionRequirement::Any
            },
            initial_user_turn: initial_user_turn.cloned(),
            expected_agent_trigger_history_item_id: None,
            expected_owner_resume_request_id: None,
            expected_latest_turn_id: Some(Some(expected_predecessor_turn_id)),
            expected_admission_revision: Some(expected_predecessor_revision),
            root_continuation_predecessor_turn_id: Some(expected_predecessor_turn_id),
            expected_remote_job_id: None,
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
            expected_latest_turn_id: None,
            expected_admission_revision: None,
            root_continuation_predecessor_turn_id: None,
            expected_remote_job_id: None,
        }
    }

    fn preserve_goal_after_latest_turn(
        turn_id: TurnId,
        initial_user_turn: Option<&UserTurn>,
        expected_latest_turn_id: Option<TurnId>,
        expected_admission_revision: u64,
    ) -> Self {
        Self {
            expected_latest_turn_id: Some(expected_latest_turn_id),
            expected_admission_revision: Some(expected_admission_revision),
            ..Self::preserve_goal(turn_id, initial_user_turn)
        }
    }

    fn set_goal_objective_after_latest_turn(
        turn_id: TurnId,
        objective: impl Into<String>,
        initial_user_turn: Option<&UserTurn>,
        expected_latest_turn_id: Option<TurnId>,
        expected_admission_revision: u64,
    ) -> Self {
        Self {
            expected_latest_turn_id: Some(expected_latest_turn_id),
            expected_admission_revision: Some(expected_admission_revision),
            ..Self::set_goal_objective(turn_id, objective, initial_user_turn)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmittedTerminalCommit {
    Applied,
    AlreadyTerminalizedBySameAdmission,
    NotOwned,
}

/// The durable terminal selected by the storage transaction that owns final settlement.
///
/// `terminal` may differ from the caller's requested value when a committed exact execution
/// interrupt or recovery fence wins the terminal linearization race.
#[derive(Debug, Clone)]
pub(crate) enum AdmittedTerminalSettlement {
    Applied { terminal: DurableTurnTerminal },
    AlreadyTerminalizedBySameAdmission { terminal: DurableTurnTerminal },
    NotOwned,
}

#[derive(Debug, Clone)]
pub enum RunAdmissionLeaseRenewalOutcome {
    Renewed,
    InterruptRequested(crate::protocol::TurnInterruptionCause),
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

impl AdmittedTerminalSettlement {
    pub(crate) fn commit(&self) -> AdmittedTerminalCommit {
        match self {
            Self::Applied { .. } => AdmittedTerminalCommit::Applied,
            Self::AlreadyTerminalizedBySameAdmission { .. } => {
                AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission
            }
            Self::NotOwned => AdmittedTerminalCommit::NotOwned,
        }
    }

    pub(crate) fn into_terminal(self) -> Option<DurableTurnTerminal> {
        match self {
            Self::Applied { terminal } | Self::AlreadyTerminalizedBySameAdmission { terminal } => {
                Some(terminal)
            }
            Self::NotOwned => None,
        }
    }
}

impl SqliteSessionRepository {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
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
                     sessions.provider_connection_json,
                     sessions.created_at_ms, sessions.updated_at_ms,
                     sessions.completed_at_ms, sessions.session_settings_revision,
                     sessions.status,
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
                   AND NOT EXISTS (
                       SELECT 1 FROM shared_run_checkpoints AS checkpoint
                       WHERE checkpoint.session_id = sessions.id
                         AND checkpoint.turn_id = sessions.active_turn_id
                         AND checkpoint.admission_id = sessions.active_run_id
                         AND checkpoint.state = 'paused'
                   )
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
                        raw_session_runtime_state_from_row(row, 14)?,
                        row.get::<_, i64>(20)?,
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
        let admission_revision =
            session_admission_revision_in_connection(&transaction, session_id)?;
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
        let turn_progress = active_turn_position
            .or(protocol.latest_turn_position)
            .map(|(turn_id, _)| turn_progress_in_connection(&transaction, session_id, turn_id))
            .transpose()?
            .flatten();
        transaction.commit()?;
        Ok(CanonicalSessionStorageSnapshot {
            session,
            protocol,
            active_turn_position,
            pending_turn_inputs,
            admission_revision,
            turn_progress,
        })
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
            layout,
            clm_checkpoint,
            preserved_user_messages,
            summary,
            replacement_item_ids,
            ..
        } = event
        else {
            return Err(StorageError::Message(
                "compaction writer requires a CompactionCompleted event".to_string(),
            ));
        };
        layout
            .validate_checkpoint(clm_checkpoint.as_ref(), preserved_user_messages)
            .map_err(StorageError::Message)?;
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

    /// Admits a user-owned turn only if the session is still idle at the exact
    /// canonical turn generation captured by the interactive surface.
    pub async fn admit_session_turn_with_initial_user_turn_after_latest(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        initial_user_turn: Option<&UserTurn>,
        expected_latest_turn_id: Option<TurnId>,
        expected_admission_revision: u64,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::preserve_goal_after_latest_turn(
                    turn_id,
                    initial_user_turn,
                    expected_latest_turn_id,
                    expected_admission_revision,
                ),
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

    pub(crate) async fn admit_session_turn_with_initial_user_turn_and_transaction_commit<T, F>(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        initial_user_turn: &UserTurn,
        transaction_commit: F,
    ) -> Result<Option<(AdmittedTurnSnapshot, T)>, StorageError>
    where
        T: Send,
        F: FnOnce(&Transaction<'_>) -> Result<T, StorageError> + Send,
    {
        let (admission, committed) = self
            .admit_session_turn_request_at_with_transaction_commit(
                session_id,
                TurnAdmissionRequest::preserve_goal(turn_id, Some(initial_user_turn)),
                SystemClock::now_ms(),
                RUN_ADMISSION_LEASE_DURATION_MS,
                transaction_commit,
            )
            .await?;
        match admission {
            ActiveGoalTurnAdmission::Admitted(snapshot) => Ok(Some((
                snapshot,
                committed.expect("admitted transaction invoked its commit extension"),
            ))),
            ActiveGoalTurnAdmission::Unavailable => Ok(None),
            ActiveGoalTurnAdmission::GoalInactive => {
                unreachable!("unconditional admission cannot reject an inactive goal")
            }
        }
    }

    pub(crate) async fn admit_remote_task(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        job_id: ulid::Ulid,
        initial_user_turn: &UserTurn,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        let mut request = TurnAdmissionRequest::preserve_goal_after_latest_turn(
            turn_id,
            Some(initial_user_turn),
            None,
            0,
        );
        request.expected_remote_job_id = Some(job_id);
        match self
            .admit_session_turn_request_at(
                session_id,
                request,
                SystemClock::now_ms(),
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await?
        {
            ActiveGoalTurnAdmission::Admitted(snapshot) => Ok(Some(snapshot)),
            ActiveGoalTurnAdmission::Unavailable => Ok(None),
            ActiveGoalTurnAdmission::GoalInactive => {
                unreachable!("remote job has no goal prerequisite")
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

    pub async fn admit_active_goal_continuation_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        expected_predecessor_turn_id: TurnId,
        expected_predecessor_revision: u64,
    ) -> Result<ActiveGoalTurnAdmission, StorageError> {
        self.admit_active_goal_continuation_turn_with_initial_user_turn(
            session_id,
            turn_id,
            None,
            expected_predecessor_turn_id,
            expected_predecessor_revision,
        )
        .await
    }

    pub async fn admit_active_goal_continuation_turn_with_initial_user_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        initial_user_turn: Option<&UserTurn>,
        expected_predecessor_turn_id: TurnId,
        expected_predecessor_revision: u64,
    ) -> Result<ActiveGoalTurnAdmission, StorageError> {
        self.admit_root_continuation_turn_with_initial_user_turn(
            session_id,
            turn_id,
            initial_user_turn,
            expected_predecessor_turn_id,
            expected_predecessor_revision,
            true,
        )
        .await
    }

    pub async fn admit_root_continuation_turn_with_initial_user_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        initial_user_turn: Option<&UserTurn>,
        expected_predecessor_turn_id: TurnId,
        expected_predecessor_revision: u64,
        require_active_goal: bool,
    ) -> Result<ActiveGoalTurnAdmission, StorageError> {
        self.admit_session_turn_request_at(
            session_id,
            TurnAdmissionRequest::root_continuation_after_turn(
                turn_id,
                initial_user_turn,
                expected_predecessor_turn_id,
                expected_predecessor_revision,
                require_active_goal,
            ),
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

    pub async fn admit_session_turn_with_goal_objective_and_initial_user_turn_after_latest(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        objective: impl Into<String>,
        initial_user_turn: Option<&UserTurn>,
        expected_latest_turn_id: Option<TurnId>,
        expected_admission_revision: u64,
    ) -> Result<Option<AdmittedTurnSnapshot>, StorageError> {
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::set_goal_objective_after_latest_turn(
                    turn_id,
                    objective,
                    initial_user_turn,
                    expected_latest_turn_id,
                    expected_admission_revision,
                ),
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
        Ok(self
            .admit_session_turn_request_at_with_transaction_commit(
                session_id,
                request,
                now_ms,
                lease_duration_ms,
                |_| Ok(()),
            )
            .await?
            .0)
    }

    async fn admit_session_turn_request_at_with_transaction_commit<T, F>(
        &self,
        session_id: SessionId,
        request: TurnAdmissionRequest,
        now_ms: i64,
        lease_duration_ms: i64,
        transaction_commit: F,
    ) -> Result<(ActiveGoalTurnAdmission, Option<T>), StorageError>
    where
        T: Send,
        F: FnOnce(&Transaction<'_>) -> Result<T, StorageError> + Send,
    {
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
        if request.goal_requirement == TurnGoalAdmissionRequirement::Active
            && request.root_continuation_predecessor_turn_id.is_none()
        {
            return Err(StorageError::Message(
                "active-goal admission must be an exact root continuation".to_string(),
            ));
        }
        if request.expected_latest_turn_id.is_some()
            != request.expected_admission_revision.is_some()
        {
            return Err(StorageError::Message(
                "exact turn admission must carry latest-turn and revision fences together"
                    .to_string(),
            ));
        }
        if let Some(expected_predecessor_turn_id) = request.root_continuation_predecessor_turn_id
            && (request.expected_latest_turn_id != Some(Some(expected_predecessor_turn_id))
                || request.expected_admission_revision.is_none())
        {
            return Err(StorageError::Message(
                "root continuation admission must carry the same exact latest predecessor fence"
                    .to_string(),
            ));
        }
        let admission_id = AdmissionId::new();
        let now = normalize_run_lease_now_ms(now_ms);
        let lease_expires_at_ms = run_lease_expiry_ms(now, lease_duration_ms);
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM shared_run_checkpoints WHERE session_id = ?1)",
            params![session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )? {
            transaction.commit()?;
            return Ok((ActiveGoalTurnAdmission::Unavailable, None));
        }
        let Some(runtime_state) = session_runtime_state_from_connection(&transaction, session_id)?
        else {
            transaction.commit()?;
            return Ok((ActiveGoalTurnAdmission::Unavailable, None));
        };
        let remote_job = transaction
            .query_row(
                "SELECT id, admitted_turn_id FROM remote_agent_jobs WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        match (remote_job, request.expected_remote_job_id) {
            (None, None) => {}
            (Some((id, None)), Some(expected)) if id == expected.to_string() => {}
            _ => {
                transaction.commit()?;
                return Ok((ActiveGoalTurnAdmission::Unavailable, None));
            }
        }
        if exact_execution_interrupt_request_in_connection(&transaction, session_id)?.is_some() {
            transaction.commit()?;
            return Ok((ActiveGoalTurnAdmission::Unavailable, None));
        }
        if let Some(expected_latest_turn_id) = request.expected_latest_turn_id {
            let actual_latest_turn_id =
                latest_protocol_turn_ids_in_transaction(&transaction, session_id, 1)?
                    .into_iter()
                    .next();
            let actual_revision =
                session_admission_revision_in_connection(&transaction, session_id)?;
            if actual_latest_turn_id != expected_latest_turn_id
                || Some(actual_revision) != request.expected_admission_revision
                || runtime_state.status == SessionStatus::Running
            {
                transaction.commit()?;
                return Ok((ActiveGoalTurnAdmission::Unavailable, None));
            }
        }
        if let Some(expected_predecessor_turn_id) = request.root_continuation_predecessor_turn_id {
            if runtime_state.status != SessionStatus::Completed
                || first_applicable_tree_stop_fence_for_turn_in_connection(
                    &transaction,
                    session_id,
                    expected_predecessor_turn_id,
                )?
                .is_some()
            {
                transaction.commit()?;
                return Ok((ActiveGoalTurnAdmission::Unavailable, None));
            }
        }
        if let Some(durable_admission) = runtime_state.admission {
            if durable_admission.is_fresh_at(now) {
                transaction.commit()?;
                return Ok((ActiveGoalTurnAdmission::Unavailable, None));
            }
            recover_expired_run_admission_in_transaction(
                &transaction,
                session_id,
                runtime_state.status,
                durable_admission,
                now,
            )?;
        }
        if !agent_work_admissible(
            &transaction,
            session_id,
            request.expected_agent_trigger_history_item_id,
            request.expected_owner_resume_request_id,
        )? {
            transaction.commit()?;
            return Ok((ActiveGoalTurnAdmission::Unavailable, None));
        }
        if request.goal_requirement == TurnGoalAdmissionRequirement::Active {
            let active_goal = stored_thread_goal_from_connection(&transaction, session_id)?
                .filter(|stored| stored.goal.status == ThreadGoalStatus::Active);
            if active_goal.is_none() {
                transaction.commit()?;
                return Ok((ActiveGoalTurnAdmission::GoalInactive, None));
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
            return Ok((ActiveGoalTurnAdmission::Unavailable, None));
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
        claim_agent_work_for_admitted_turn(
            &transaction,
            session_id,
            admission_id,
            request.turn_id,
            request.expected_agent_trigger_history_item_id,
            request.expected_owner_resume_request_id,
            now,
        )?;
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
        let admission_revision =
            session_admission_revision_in_connection(&transaction, session_id)?;
        let snapshot = AdmittedTurnSnapshot {
            admission_id,
            admission_revision,
            goal,
            initial_user_history_item_id,
        };
        let committed = transaction_commit(&transaction)?;
        if let Some(job_id) = request.expected_remote_job_id {
            let changed = transaction.execute(
                "UPDATE remote_agent_jobs SET admitted_turn_id = ?1 WHERE id = ?2 AND session_id = ?3 AND admitted_turn_id IS NULL",
                params![request.turn_id.to_string(), job_id.to_string(), session_id.to_string()],
            )?;
            if changed != 1 {
                return Err(StorageError::Message(
                    "remote job admission owner changed".into(),
                ));
            }
        }
        transaction.commit()?;
        Ok((ActiveGoalTurnAdmission::Admitted(snapshot), Some(committed)))
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

    /// Returns the exact interactive mutation fence in one SQLite snapshot.
    pub async fn active_turn_expectation_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ActiveTurnExpectation>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let Some(runtime_state) = session_runtime_state_from_connection(&transaction, session_id)?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let latest_turn_id = latest_protocol_turn_ids_in_transaction(&transaction, session_id, 1)?
            .into_iter()
            .next();
        let revision = session_admission_revision_in_connection(&transaction, session_id)?;
        let expectation = match runtime_state.stop_state() {
            DurableSessionStopState::Running(target) => ActiveTurnExpectation::Turn {
                turn_id: target.turn_id(),
                revision,
            },
            DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_) => {
                ActiveTurnExpectation::Idle {
                    latest_turn_id,
                    revision,
                }
            }
        };
        transaction.commit()?;
        Ok(Some(expectation))
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

    pub async fn active_session_for_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<SessionId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        mutation_blocker_for_project_in_connection(&connection, project_id)
    }

    #[cfg(test)]
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

    #[cfg(test)]
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

    /// Records a tree Stop boundary only while the exact UI-observed root state
    /// is still current. The check and fence append share one immediate
    /// transaction, so an A -> B replacement cannot slip between them.
    pub(crate) async fn record_agent_tree_stop_fence_for_expected_state(
        &self,
        stopped_session_id: SessionId,
        cause: crate::protocol::TurnInterruptionCause,
        expected: ActiveTurnExpectation,
    ) -> Result<Option<AgentTreeStopFence>, StorageError> {
        let cause = explicit_agent_tree_stop_fence_cause(cause)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(runtime_state) =
            session_runtime_state_from_connection(&transaction, stopped_session_id)?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let latest_turn_id =
            latest_protocol_turn_ids_in_transaction(&transaction, stopped_session_id, 1)?
                .into_iter()
                .next();
        let actual_revision =
            session_admission_revision_in_connection(&transaction, stopped_session_id)?;
        // Stop has one deliberate completion-race exception to ordinary exact-state equality.
        // A Stop captured against Turn(A) may still fence A's detached descendants after A has
        // just terminalized, provided A remains the latest canonical turn. It must never cross
        // into a replacement Turn(B). Idle expectations remain strict ABA fences.
        let expected_still_owns_stop = match (expected, runtime_state.stop_state()) {
            (
                ActiveTurnExpectation::Turn { turn_id, revision },
                DurableSessionStopState::Running(target),
            ) => actual_revision == revision && target.turn_id() == turn_id,
            (
                ActiveTurnExpectation::Turn { turn_id, revision },
                DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_),
            ) => actual_revision == revision && latest_turn_id == Some(turn_id),
            (
                ActiveTurnExpectation::Idle {
                    latest_turn_id: expected_latest,
                    revision,
                },
                DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_),
            ) => actual_revision == revision && latest_turn_id == expected_latest,
            (ActiveTurnExpectation::Idle { .. }, DurableSessionStopState::Running(_)) => false,
        };
        if !expected_still_owns_stop {
            transaction.commit()?;
            return Ok(None);
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

    /// Validates one exact durable root execution without terminalizing it.
    pub(crate) async fn validate_root_execution_at_expectation(
        &self,
        session_id: SessionId,
        expected_turn_id: TurnId,
        expected_admission_revision: u64,
    ) -> Result<ExactRootExecutionValidationState, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let runtime_state = session_runtime_state_from_connection(&transaction, session_id)?;
        let validation = exact_root_execution_validation_in_transaction(
            &transaction,
            session_id,
            runtime_state,
            expected_turn_id,
            expected_admission_revision,
        )?;
        transaction.commit()?;
        Ok(validation)
    }

    /// Persists one exact interruption intent for a live root or child admission.
    ///
    /// This compare-and-record transaction never terminalizes the turn or creates a tree fence.
    /// The admitted owner observes the immutable request through lease renewal and remains the
    /// sole authority for terminal settlement after its local commit reservations drain.
    pub(crate) async fn request_exact_execution_interrupt(
        &self,
        session_id: SessionId,
        expected_turn_id: TurnId,
        expected_admission_revision: u64,
        cause: crate::protocol::TurnInterruptionCause,
    ) -> Result<ExactExecutionInterruptRequestSettlement, StorageError> {
        let cause_text = exact_execution_interrupt_cause_text(cause)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let runtime_state = session_runtime_state_from_connection(&transaction, session_id)?;
        let active_admission = runtime_state
            .filter(|state| state.status == SessionStatus::Running)
            .and_then(|state| state.admission)
            .filter(|admission| admission.turn_id == expected_turn_id);
        let is_child = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM session_spawn_edges WHERE child_session_id = ?1
             )",
            [session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?;
        let topology_matches = match cause {
            crate::protocol::TurnInterruptionCause::UserStop => !is_child,
            crate::protocol::TurnInterruptionCause::AgentInterrupted => is_child,
            crate::protocol::TurnInterruptionCause::ApprovalAborted
            | crate::protocol::TurnInterruptionCause::TreeStopped => false,
        };
        let exact_owner_matches = active_admission.is_some()
            && session_admission_revision_in_connection(&transaction, session_id)?
                == expected_admission_revision
            && transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1
                     FROM protocol_turn_sequence_allocators
                     WHERE session_id = ?1 AND turn_id = ?2
                 )",
                params![session_id.to_string(), expected_turn_id.to_string()],
                |row| row.get::<_, bool>(0),
            )?
            && terminal_for_turn_in_connection(&transaction, session_id, expected_turn_id)?
                .is_none()
            && topology_matches;
        if !exact_owner_matches {
            transaction.commit()?;
            return Ok(ExactExecutionInterruptRequestSettlement::TargetChanged);
        }
        let active_admission =
            active_admission.expect("running exact root validation retains its durable admission");
        if let Some(existing) =
            exact_execution_interrupt_request_in_connection(&transaction, session_id)?
        {
            transaction.commit()?;
            return Ok(
                if existing.admission_id == active_admission.admission_id
                    && existing.turn_id == expected_turn_id
                    && existing.admission_revision == expected_admission_revision
                    && existing.cause == cause
                {
                    ExactExecutionInterruptRequestSettlement::AlreadyRequested
                } else {
                    ExactExecutionInterruptRequestSettlement::TargetChanged
                },
            );
        }
        transaction.execute(
            "INSERT INTO exact_execution_interrupt_requests (
                 session_id, admission_id, turn_id, admission_revision, cause, requested_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id.to_string(),
                active_admission.admission_id.to_string(),
                expected_turn_id.to_string(),
                i64::try_from(expected_admission_revision).map_err(|_| {
                    StorageError::Message(format!(
                        "exact execution interrupt request revision {expected_admission_revision} exceeds SQLite INTEGER"
                    ))
                })?,
                cause_text,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(ExactExecutionInterruptRequestSettlement::Recorded)
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
                        model_parameters_json, provider_connection_json,
                        created_at_ms, updated_at_ms, completed_at_ms,
                        session_settings_revision,
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
                   AND NOT EXISTS (
                       SELECT 1 FROM side_chat_bindings
                       WHERE conversation_session_id = sessions.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM remote_agent_jobs
                       WHERE session_id = sessions.id
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
                    model_parameters_json, provider_connection_json,
                    created_at_ms, updated_at_ms, completed_at_ms,
                    session_settings_revision,
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
               AND NOT EXISTS (
                   SELECT 1 FROM side_chat_bindings
                   WHERE conversation_session_id = sessions.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM remote_agent_jobs
                   WHERE session_id = sessions.id
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
                    model_parameters_json, provider_connection_json,
                    created_at_ms, updated_at_ms, completed_at_ms,
                    session_settings_revision,
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
               AND NOT EXISTS (
                   SELECT 1 FROM side_chat_bindings
                   WHERE conversation_session_id = sessions.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM remote_agent_jobs
                   WHERE session_id = sessions.id
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
                    model_parameters_json, provider_connection_json,
                    created_at_ms, updated_at_ms, completed_at_ms,
                    session_settings_revision,
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
               AND NOT EXISTS (
                   SELECT 1 FROM side_chat_bindings
                   WHERE conversation_session_id = sessions.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM remote_agent_jobs
                   WHERE session_id = sessions.id
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
        let next_provider_connection = provider_connection_for_settings_update(
            &current,
            &next_base_url,
            patch.provider_connection.as_ref(),
        );
        let next_model_parameters = patch.apply_to_model_parameters(&current.model_parameters);
        let changed = next_cwd != current.cwd
            || next_model != current.model
            || next_base_url != current.base_url
            || next_access_mode != current.access_mode
            || next_provider_connection != current.provider_connection
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
        if current.session_settings_revision >= i64::MAX as u64 {
            return Err(StorageError::Message(format!(
                "session {id} settings revision is exhausted"
            )));
        }
        let now = SystemClock::now_ms().max(current.updated_at_ms.saturating_add(1));
        let updated = transaction.execute(
            "UPDATE sessions
             SET cwd_path = ?2, model_name = ?3, base_url = ?4, access_mode = ?5,
                 model_parameters_json = ?6, provider_connection_json = ?7,
                 updated_at_ms = ?8,
                 session_settings_revision = session_settings_revision + 1
             WHERE id = ?1
               AND session_settings_revision < 9223372036854775807",
            params![
                id.to_string(),
                next_cwd.as_str(),
                next_model,
                next_base_url,
                next_access_mode.as_str(),
                serde_json::to_string(&next_model_parameters)?,
                serialize_provider_connection(&next_provider_connection)?,
                now,
            ],
        )?;
        if updated != 1 {
            return Err(StorageError::Message(format!(
                "session {id} settings revision could not advance"
            )));
        }
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

fn parse_session_settings_revision_column(
    row: &rusqlite::Row<'_>,
    column_index: usize,
) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(column_index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column_index,
            rusqlite::types::Type::Integer,
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
        provider_connection: parse_provider_connection_column(row, 9)?,
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
        completed_at_ms: row.get(12)?,
        session_settings_revision: parse_session_settings_revision_column(row, 13)?,
    })
}

fn session_record_with_raw_runtime_state_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(SessionRecord, RawSessionRuntimeState)> {
    let session = session_record_with_identity_from_row(row)?;
    let raw = raw_session_runtime_state_from_row(row, 14)?;
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

pub(crate) fn insert_session_in_transaction(
    transaction: &Transaction<'_>,
    id: SessionId,
    draft: &NewSession,
    now_ms: i64,
) -> Result<SessionRecord, StorageError> {
    transaction.execute(
        "INSERT INTO sessions (id, project_id, title, status, cwd_path, model_name, base_url, access_mode, model_parameters_json, provider_connection_json, created_at_ms, updated_at_ms, completed_at_ms)
         VALUES (?1, ?2, ?3, 'idle', ?4, ?5, ?6, ?7, '{}', ?8, ?9, ?9, NULL)",
        params![
            id.to_string(),
            draft.project_id.to_string(),
            draft.title.as_str(),
            draft.cwd.as_str(),
            draft.model.as_str(),
            draft.base_url.as_str(),
            draft.access_mode.as_str(),
            serialize_provider_connection(&draft.provider_connection)?,
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

pub(crate) fn session_record_from_connection(
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
                    model_parameters_json, provider_connection_json,
                    created_at_ms, updated_at_ms, completed_at_ms,
                    session_settings_revision,
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
                        provider_connection: parse_provider_connection_column(row, 8)?,
                        created_at_ms: row.get(9)?,
                        updated_at_ms: row.get(10)?,
                        completed_at_ms: row.get(11)?,
                        session_settings_revision: parse_session_settings_revision_column(row, 12)?,
                    },
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, Option<String>>(17)?,
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

fn turn_progress_in_connection(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<crate::session::model::CanonicalTurnProgress>, StorageError> {
    // Aggregate canonical events inside the caller's read transaction. The UI's
    // paged history and replayed wakeups never own or increment these counts.
    let progress = connection.query_row(
        "WITH events AS (
             SELECT sequence_no, msg_json AS payload_json FROM protocol_runtime_events
             WHERE session_id = ?1 AND turn_id = ?2
         ), tool_states AS (
             SELECT json_extract(payload_json, '$.envelope.status') AS status,
                    json_extract(payload_json, '$.envelope.success') AS success,
                    ROW_NUMBER() OVER (
                        PARTITION BY json_extract(payload_json, '$.envelope.call_id')
                        ORDER BY sequence_no DESC
                    ) AS latest
             FROM events WHERE json_extract(payload_json, '$.kind') = 'tool_lifecycle'
         )
         SELECT
             (SELECT COUNT(*) FROM events WHERE json_extract(payload_json, '$.kind') = 'model_request_prepared'),
             COUNT(*),
             COUNT(*) FILTER (WHERE status = 'completed' AND COALESCE(success, 1) != 0),
             COUNT(*) FILTER (WHERE status = 'declined'),
             COUNT(*) FILTER (WHERE status = 'cancelled'),
             COUNT(*) FILTER (WHERE status = 'failed' OR (status = 'completed' AND success = 0)),
             (SELECT COUNT(*) FROM events WHERE json_extract(payload_json, '$.kind') = 'context_compacted')
         FROM tool_states WHERE latest = 1",
        params![session_id.to_string(), turn_id.to_string()],
        |row| Ok(crate::session::model::CanonicalTurnProgress {
            turn_id,
            model_request_count: row.get(0)?,
            tool_call_count: row.get(1)?,
            completed_tool_count: row.get(2)?,
            declined_tool_count: row.get(3)?,
            cancelled_tool_count: row.get(4)?,
            failed_tool_count: row.get(5)?,
            compaction_count: row.get(6)?,
        }),
    ).map_err(StorageError::from)?;
    // Migrated canonical tool evidence can predate runtime lifecycle events.
    // A partial aggregate must not erase a call still owned by that history.
    if connection.query_row(
        "WITH canonical_calls AS (
             SELECT json_extract(payload_json, '$.call_id') AS call_id
             FROM protocol_history_items
             WHERE session_id = ?1 AND turn_id = ?2
               AND json_extract(payload_json, '$.kind') IN ('tool_call', 'tool_output')
             UNION
             SELECT json_extract(payload_json, '$.call_id') AS call_id
             FROM protocol_turn_items
             WHERE session_id = ?1 AND turn_id = ?2
               AND json_extract(payload_json, '$.call_id') IS NOT NULL
         ) SELECT EXISTS (
             SELECT 1 FROM canonical_calls AS canonical
             WHERE NOT EXISTS (
                 SELECT 1 FROM protocol_runtime_events AS runtime
                 WHERE runtime.session_id = ?1 AND runtime.turn_id = ?2
                   AND json_extract(runtime.msg_json, '$.kind') = 'tool_lifecycle'
                   AND json_extract(runtime.msg_json, '$.envelope.call_id') = canonical.call_id
             )
         )",
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(None);
    }
    Ok(Some(progress))
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

fn sha256_payload(payload_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload_json.as_bytes());
    format!("{:x}", hasher.finalize())
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

fn session_admission_revision_in_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<u64, StorageError> {
    let revision = connection
        .query_row(
            "SELECT revision FROM session_admission_revisions WHERE session_id = ?1",
            params![session_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "session {session_id} has no canonical admission revision"
            ))
        })?;
    u64::try_from(revision).map_err(|_| {
        StorageError::Message(format!(
            "session {session_id} has invalid admission revision {revision}"
        ))
    })
}

fn exact_root_execution_validation_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    runtime_state: Option<ValidatedSessionRuntimeState>,
    expected_turn_id: TurnId,
    expected_admission_revision: u64,
) -> Result<ExactRootExecutionValidationState, StorageError> {
    let Some(runtime_state) = runtime_state else {
        return Ok(ExactRootExecutionValidationState::TargetChanged);
    };
    let actual_revision = session_admission_revision_in_connection(transaction, session_id)?;
    let latest_turn_id = latest_protocol_turn_ids_in_transaction(transaction, session_id, 1)?
        .into_iter()
        .next();
    if actual_revision != expected_admission_revision || latest_turn_id != Some(expected_turn_id) {
        return Ok(ExactRootExecutionValidationState::TargetChanged);
    }
    Ok(match runtime_state.stop_state() {
        DurableSessionStopState::Running(target) if target.turn_id() == expected_turn_id => {
            ExactRootExecutionValidationState::Running
        }
        DurableSessionStopState::Running(_) => ExactRootExecutionValidationState::TargetChanged,
        DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_) => {
            ExactRootExecutionValidationState::Terminal
        }
    })
}

fn exact_execution_interrupt_request_in_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Option<DurableExactExecutionInterruptRequest>, StorageError> {
    let row = connection
        .query_row(
            "SELECT admission_id, turn_id, admission_revision, cause
             FROM exact_execution_interrupt_requests
             WHERE session_id = ?1",
            [session_id.to_string()],
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
    let Some((admission_id, turn_id, admission_revision, cause)) = row else {
        return Ok(None);
    };
    let admission_id = admission_id.parse::<AdmissionId>().map_err(|error| {
        StorageError::Message(format!(
            "session {session_id} exact execution interrupt request has invalid admission id: {error}"
        ))
    })?;
    let turn_id = turn_id.parse::<TurnId>().map_err(|error| {
        StorageError::Message(format!(
            "session {session_id} exact execution interrupt request has invalid turn id: {error}"
        ))
    })?;
    let admission_revision = u64::try_from(admission_revision).map_err(|_| {
        StorageError::Message(format!(
            "session {session_id} exact execution interrupt request has invalid admission revision {admission_revision}"
        ))
    })?;
    let cause = match cause.as_str() {
        "user_stop" => crate::protocol::TurnInterruptionCause::UserStop,
        "agent_interrupted" => crate::protocol::TurnInterruptionCause::AgentInterrupted,
        _ => {
            return Err(StorageError::Message(format!(
                "session {session_id} exact execution interrupt request has invalid cause `{cause}`"
            )));
        }
    };
    Ok(Some(DurableExactExecutionInterruptRequest {
        admission_id,
        turn_id,
        admission_revision,
        cause,
    }))
}

fn exact_execution_interrupt_request_for_admission_in_connection(
    connection: &Connection,
    session_id: SessionId,
    admission: DurableRunAdmission,
) -> Result<Option<DurableExactExecutionInterruptRequest>, StorageError> {
    let Some(request) = exact_execution_interrupt_request_in_connection(connection, session_id)?
    else {
        return Ok(None);
    };
    let admission_revision = session_admission_revision_in_connection(connection, session_id)?;
    Ok((request.admission_id == admission.admission_id
        && request.turn_id == admission.turn_id
        && request.admission_revision == admission_revision)
        .then_some(request))
}

fn exact_execution_interrupt_cause_text(
    cause: crate::protocol::TurnInterruptionCause,
) -> Result<&'static str, StorageError> {
    match cause {
        crate::protocol::TurnInterruptionCause::UserStop => Ok("user_stop"),
        crate::protocol::TurnInterruptionCause::AgentInterrupted => Ok("agent_interrupted"),
        crate::protocol::TurnInterruptionCause::ApprovalAborted
        | crate::protocol::TurnInterruptionCause::TreeStopped => Err(StorageError::Message(
            "exact execution interrupt request only accepts UserStop or AgentInterrupted"
                .to_string(),
        )),
    }
}

fn ensure_idle_latest_turn_expectation_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    expected_latest_turn_id: Option<TurnId>,
    expected_revision: u64,
) -> Result<(), StorageError> {
    let runtime_state = session_runtime_state_from_connection(transaction, session_id)?
        .ok_or_else(|| StorageError::Message(format!("session {session_id} was not found")))?;
    let actual_latest_turn_id =
        latest_protocol_turn_ids_in_transaction(transaction, session_id, 1)?
            .into_iter()
            .next();
    let actual_revision = session_admission_revision_in_connection(transaction, session_id)?;
    if runtime_state.status == SessionStatus::Running
        || actual_latest_turn_id != expected_latest_turn_id
        || actual_revision != expected_revision
    {
        return Err(StorageError::Message(format!(
            "session {session_id} active-turn owner changed before the goal mutation"
        )));
    }
    Ok(())
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

fn provider_connection_for_settings_update(
    current: &SessionRecord,
    next_base_url: &str,
    explicit: Option<&SessionProviderConnection>,
) -> Option<SessionProviderConnection> {
    if let Some(explicit) = explicit {
        return Some(explicit.clone());
    }
    current.provider_connection.as_ref().map(|connection| {
        if current.base_url == next_base_url {
            connection.clone()
        } else {
            connection.without_credentials()
        }
    })
}

fn serialize_provider_connection(
    value: &Option<SessionProviderConnection>,
) -> Result<Option<String>, StorageError> {
    let Some(value) = value else {
        return Ok(None);
    };
    value.validate().map_err(StorageError::Message)?;
    #[derive(serde::Serialize)]
    struct PersistedProviderConnection<'a> {
        profile: crate::config::ProviderProfile,
        api_key_env: Option<&'a str>,
        extra_headers: &'a BTreeMap<String, String>,
    }
    Ok(Some(serde_json::to_string(&PersistedProviderConnection {
        profile: value.profile,
        api_key_env: value.api_key_env.as_deref(),
        extra_headers: &value.extra_headers,
    })?))
}

fn parse_provider_connection_column(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<SessionProviderConnection>> {
    let Some(value) = row.get::<_, Option<String>>(column)? else {
        return Ok(None);
    };
    let connection =
        serde_json::from_str::<SessionProviderConnection>(&value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    connection.validate().map_err(|message| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
        )
    })?;
    Ok(Some(connection))
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

pub(crate) fn session_stop_state_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Option<DurableSessionStopState>, StorageError> {
    session_runtime_state_from_connection(connection, session_id)
        .map(|state| state.map(ValidatedSessionRuntimeState::stop_state))
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

pub(crate) fn normalize_run_lease_now_ms(now_ms: i64) -> i64 {
    now_ms.clamp(0, i64::MAX - 1)
}

fn run_lease_expiry_ms(now_ms: i64, lease_duration_ms: i64) -> i64 {
    normalize_run_lease_now_ms(now_ms).saturating_add(lease_duration_ms.max(1))
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
        || terminal.metrics.cumulative_token_usage.is_some()
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
    if was_active && !matches!(recovery_outcome, TurnTerminalOutcome::Interrupted { .. }) {
        deliver_all_pending_turn_steers_in_transaction(
            transaction,
            session_id,
            recovery_admission.admission_id,
            recovery_turn_id,
            now_ms,
        )?;
    }
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
            discard_all_pending_turn_steers_in_transaction(
                transaction,
                session_id,
                recovery_admission.admission_id,
                turn_id,
                resolver_terminal_event_id,
                now_ms,
            )?;
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
        // A recovered lease cannot publish or defer work from a generation that a durable
        // tree Stop already closed.
        publish_or_defer_terminal_handoff(
            transaction,
            TerminalHandoff {
                session_id,
                turn_id,
                tree_stop_fenced: recovery_turn_is_tree_stop_fenced,
                deferred: retained_parent
                    .filter(|_| {
                        has_durable_descendant_work
                            || pending_direct_child_result.is_some()
                            || repended_owner_resume
                    })
                    .map(|parent| (parent, DeferredAgentCompletionKind::CrashFailed)),
                pending_direct_child_result,
                owner_resume_pending: repended_owner_resume,
                root_label: "expired deferred owner tree root",
                now_ms,
            },
            || validate_terminal_event(session_id, &event),
        )?;
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
mod tests;
