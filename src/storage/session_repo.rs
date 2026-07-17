use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::config::{AccessMode, ProviderEndpoint};
use crate::error::StorageError;
use crate::protocol::{
    CanonicalProtocolSnapshot, HistoryItem, HistoryItemId, HistoryItemPayload,
    InterAgentCommunication, ModelResponseId, ProtocolPageRequest, RuntimeEvent, RuntimeEventId,
    RuntimeEventMsg, SteerTurn, TurnId, TurnItem, TurnItemId, TurnItemPayload, TurnTerminalOutcome,
    UserTurn, canonical_protocol_snapshot_from_connection, fork_canonical_items_in_transaction,
    insert_idle_inter_agent_history_in_transaction,
    insert_session_owned_event_bundle_in_transaction, latest_protocol_turn_ids_in_transaction,
    project_inter_agent_communication, project_protocol_run_event,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableSessionStopState {
    Idle,
    Running(RunningSessionTerminalTarget),
    Terminal(SessionStatus),
}

#[derive(Debug, Clone)]
pub(crate) struct RunningSessionRecoveryCandidate {
    pub session: SessionRecord,
    pub terminal_target: RunningSessionTerminalTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalOwnerGuard {
    Admitted {
        admission_id: AdmissionId,
        turn_id: TurnId,
    },
    Captured(RunningSessionTerminalTarget),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectChildRunAdmissionState {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TurnAdmissionRequest {
    turn_id: TurnId,
    goal_change: TurnGoalAdmissionChange,
    goal_requirement: TurnGoalAdmissionRequirement,
}

impl TurnAdmissionRequest {
    fn preserve_goal(turn_id: TurnId) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::Preserve,
            goal_requirement: TurnGoalAdmissionRequirement::Any,
        }
    }

    fn require_active_goal(turn_id: TurnId) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::Preserve,
            goal_requirement: TurnGoalAdmissionRequirement::Active,
        }
    }

    fn set_goal_objective(turn_id: TurnId, objective: impl Into<String>) -> Self {
        Self {
            turn_id,
            goal_change: TurnGoalAdmissionChange::SetObjective(objective.into()),
            goal_requirement: TurnGoalAdmissionRequirement::Any,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmittedTerminalCommit {
    Applied,
    AlreadyTerminalizedBySameAdmission,
    UnseenSteer { expected: usize, actual: usize },
    UnseenAgentCommunication { expected: usize, actual: usize },
    NotOwned,
}

#[derive(Debug, Clone)]
pub enum RunAdmissionLeaseRenewalOutcome {
    Renewed,
    Terminal(crate::session::model::DurableTurnTerminal),
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

    pub async fn insert_session_spawn_edge(
        &self,
        root_session_id: SessionId,
        parent_session_id: SessionId,
        child_session_id: SessionId,
        agent_path: &str,
        task_name: &str,
    ) -> Result<SessionSpawnEdge, StorageError> {
        validate_flat_session_spawn_edge(
            root_session_id,
            parent_session_id,
            child_session_id,
            agent_path,
            task_name,
        )
        .map_err(StorageError::Message)?;
        let edge = SessionSpawnEdge {
            root_session_id,
            parent_session_id,
            child_session_id,
            agent_path: agent_path.to_string(),
            task_name: task_name.to_string(),
            created_at_ms: SystemClock::now_ms(),
        };
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO session_spawn_edges
             (root_session_id, parent_session_id, child_session_id, agent_path, task_name, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                edge.root_session_id.to_string(),
                edge.parent_session_id.to_string(),
                edge.child_session_id.to_string(),
                edge.agent_path,
                edge.task_name,
                edge.created_at_ms,
            ],
        )?;
        Ok(edge)
    }

    pub async fn session_spawn_edge_for_child(
        &self,
        child_session_id: SessionId,
    ) -> Result<Option<SessionSpawnEdge>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .query_row(
                "SELECT root_session_id, parent_session_id, child_session_id,
                        agent_path, task_name, created_at_ms
                 FROM session_spawn_edges
                 WHERE child_session_id = ?1",
                params![child_session_id.to_string()],
                session_spawn_edge_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub async fn list_session_spawn_edges(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<SessionSpawnEdge>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT root_session_id, parent_session_id, child_session_id,
                    agent_path, task_name, created_at_ms
             FROM session_spawn_edges
             WHERE root_session_id = ?1
             ORDER BY created_at_ms ASC, child_session_id ASC",
        )?;
        statement
            .query_map(
                params![root_session_id.to_string()],
                session_spawn_edge_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Reads every retained direct child and its validated durable runtime state in one SQL
    /// snapshot. The caller may combine this semantic projection with its process-local run
    /// registry without issuing one query per child.
    pub async fn list_direct_child_run_admission_states(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<DirectChildRunAdmissionState>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT edge.root_session_id, edge.parent_session_id, edge.child_session_id,
                     edge.agent_path, edge.task_name, edge.created_at_ms,
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
             ORDER BY edge.created_at_ms ASC, edge.child_session_id ASC",
        )?;
        let rows = statement
            .query_map(params![root_session_id.to_string()], |row| {
                Ok((
                    session_spawn_edge_from_row(row)?,
                    raw_session_runtime_state_from_row(row, 6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(edge, raw)| {
                let runtime_state = validate_raw_session_runtime_state(edge.child_session_id, raw)?;
                Ok(DirectChildRunAdmissionState {
                    edge,
                    blocks_new_root_turn: runtime_state.blocks_mutation_at(now),
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
        after: Option<SessionId>,
        through: SessionId,
        limit: usize,
    ) -> Result<Vec<RunningSessionRecoveryCandidate>, StorageError> {
        let limit = sqlite_limit(limit)?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let (sql, after_text) = match after {
            Some(after) => (
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
                 WHERE status = 'running' AND id > ?1 AND id <= ?2
                 ORDER BY id ASC
                 LIMIT ?3",
                Some(after.to_string()),
            ),
            None => (
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
                 WHERE status = 'running' AND id <= ?2
                 ORDER BY id ASC
                 LIMIT ?3",
                None,
            ),
        };
        let mut statement = connection.prepare(sql)?;
        let rows = statement
            .query_map(params![after_text, through.to_string(), limit], |row| {
                Ok((
                    session_record_with_identity_from_row(row)?,
                    raw_session_runtime_state_from_row(row, 12)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(session, raw)| {
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
        if let Some(active_session_id) = active_session_for_mutation_branch(
            &transaction,
            session_id,
            true,
            normalize_run_lease_now_ms(SystemClock::now_ms()),
        )? {
            return Err(StorageError::Message(format!(
                "session {session_id} has active agent-tree session {active_session_id}; stop the agent tree before deleting it"
            )));
        }
        let is_direct_child = transaction
            .query_row(
                "SELECT 1 FROM session_spawn_edges WHERE child_session_id = ?1",
                params![session_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let mut deleted_session_ids = if is_direct_child {
            Vec::new()
        } else {
            let mut statement = transaction.prepare(
                "SELECT child_session_id
                 FROM session_spawn_edges
                 WHERE root_session_id = ?1
                 ORDER BY created_at_ms ASC, child_session_id ASC",
            )?;
            let children = statement
                .query_map(params![session_id.to_string()], |row| {
                    parse_session_id_column(row, 0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            children
        };
        deleted_session_ids.push(session_id);

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
            active_session_for_mutation_branch(&transaction, root_session_id, true, now)?;
        if let Some(active_tree_session) = active_tree_session {
            return Err(StorageError::Message(format!(
                "session {session_id} belongs to agent tree {root_session_id}, which still has active session {active_tree_session}; stop the complete agent tree before rollback"
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

    pub async fn append_user_turn_with_protocol_bundle(
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

    pub fn append_inter_agent_communication_with_protocol_bundle(
        &self,
        session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
    ) -> Result<HistoryItemId, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
            Some(admission.turn_id)
        } else {
            None
        };
        let has_active_admission = active_turn_id.is_some();
        if require_active_recipient && !has_active_admission {
            return Err(StorageError::Message(format!(
                "recipient session {session_id} became terminal before inter-agent communication could be committed"
            )));
        }
        let history_item_id = match active_turn_id {
            Some(turn_id) => {
                let projection =
                    project_inter_agent_communication(session_id, turn_id, 0, communication);
                insert_session_owned_event_bundle_in_transaction(
                    &SESSION_PROTOCOL_WRITE_AUTHORITY,
                    &transaction,
                    &projection.runtime_event,
                    projection.history_item.as_ref(),
                    projection.turn_item.as_ref(),
                )?
                .history_item
                .ok_or_else(|| {
                    StorageError::Message(
                        "inter-agent communication projection omitted canonical history"
                            .to_string(),
                    )
                })?
                .id
            }
            None => insert_idle_inter_agent_history_in_transaction(
                &SESSION_PROTOCOL_WRITE_AUTHORITY,
                &transaction,
                session_id,
                communication,
            )?,
        };
        transaction.commit()?;
        Ok(history_item_id)
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
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::preserve_goal(turn_id),
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

    pub async fn admit_active_goal_continuation_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<ActiveGoalTurnAdmission, StorageError> {
        self.admit_session_turn_request_at(
            session_id,
            TurnAdmissionRequest::require_active_goal(turn_id),
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
        match self
            .admit_session_turn_request_at(
                session_id,
                TurnAdmissionRequest::set_goal_objective(turn_id, objective),
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
                TurnAdmissionRequest::preserve_goal(turn_id),
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
        let now = normalize_run_lease_now_ms(now_ms);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(None);
        };
        Ok(runtime_state
            .fresh_admission_at(now)
            .filter(|admission| {
                admission.admission_id == admission_id && admission.turn_id == turn_id
            })
            .map(|_| runtime_state.status))
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
        Ok(runtime_state.fresh_admission_at(now).is_some())
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
        Ok(runtime_state.fresh_running_turn_at(now))
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
        Ok(runtime_state.blocks_mutation_at(now))
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
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        active_session_for_mutation_branch(&connection, session_id, true, now)
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
        if active_turn_id != steer.expected_turn_id {
            return Err(StorageError::Message(format!(
                "expected active turn id `{}` but current active turn id is `{active_turn_id}`",
                steer.expected_turn_id
            )));
        }

        let history_item = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: crate::protocol::HistoryScope::Turn {
                turn_id: active_turn_id,
            },
            sequence_no: 0,
            created_at_ms: now,
            payload: HistoryItemPayload::SteerTurn {
                expected_turn_id: active_turn_id,
                content: steer.content_parts(),
                additional_context: steer.additional_context.clone(),
                client_user_message_id: steer.client_user_message_id.clone(),
            },
        };
        let turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id: active_turn_id,
            source_item_id: Some(history_item.id),
            sequence_no: 0,
            payload: TurnItemPayload::SteerMessage { text: steer.text() },
        };
        let event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id: active_turn_id,
            sequence_no: 0,
            created_at_ms: now,
            msg: RuntimeEventMsg::SteerInputAccepted {
                item_count: steer.items.len(),
                client_user_message_id: steer.client_user_message_id.clone(),
            },
        };
        let stored = insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            &transaction,
            &event,
            Some(&history_item),
            Some(&turn_item),
        )?;
        transaction.commit()?;
        Ok(stored
            .history_item
            .expect("steer bundle includes history item")
            .id)
    }

    pub async fn active_session_for_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<SessionId>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
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
        let mut first_blocker = None;
        while let Some(row) = rows.next()? {
            let session_id = parse_session_id_column(row, 0)?;
            let runtime_state = validate_raw_session_runtime_state(
                session_id,
                raw_session_runtime_state_from_row(row, 1)?,
            )?;
            if first_blocker.is_none() && runtime_state.blocks_mutation_at(now) {
                first_blocker = Some(session_id);
            }
        }
        Ok(first_blocker)
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
                None,
                None,
                None,
            )
            .await?
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
                None,
                None,
                None,
            )
            .await?
            .was_applied())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn terminalize_admitted_turn_with_protocol_event(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        expected_seen_steer_count: Option<usize>,
        expected_seen_agent_communication_count: Option<usize>,
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
            expected_seen_steer_count,
            expected_seen_agent_communication_count,
            expected_active_goal_id_to_block,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn terminalize_turn_with_protocol_event_guarded(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        owner_guard: TerminalOwnerGuard,
        protocol_sequence_no: Option<i64>,
        retain_active_admission: bool,
        expected_seen_steer_count: Option<usize>,
        expected_seen_agent_communication_count: Option<usize>,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        let terminal = validate_terminal_event(session_id, event)?;
        let status = terminal.session_status();
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let Some(runtime_state) = session_runtime_state_from_connection(&transaction, session_id)?
        else {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::NotOwned);
        };
        let Some(durable_admission) = runtime_state.admission else {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::NotOwned);
        };
        let admitted_guard = matches!(owner_guard, TerminalOwnerGuard::Admitted { .. });
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
        };
        if !owner_matches {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::NotOwned);
        }
        let protocol_turn_id = durable_admission.turn_id;
        let admission_id_text = durable_admission.admission_id.to_string();
        if runtime_state.status != SessionStatus::Running {
            terminal_for_retained_admission_in_connection(
                &transaction,
                session_id,
                runtime_state.status,
                durable_admission,
            )?;
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
                return Ok(AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission);
            }
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::NotOwned);
        }
        if terminal_for_turn_in_connection(&transaction, session_id, protocol_turn_id)?.is_some() {
            return Err(StorageError::Message(format!(
                "running session {session_id} active turn {protocol_turn_id} already has a durable terminal"
            )));
        }

        if let Some(expected) = expected_seen_steer_count {
            let actual = count_steer_history_items(&transaction, session_id)?;
            if actual != expected {
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::UnseenSteer { expected, actual });
            }
        }
        if let Some(expected) = expected_seen_agent_communication_count {
            let actual = count_agent_communication_history_items(&transaction, session_id)?;
            if actual != expected {
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::UnseenAgentCommunication { expected, actual });
            }
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
            return Ok(AdmittedTerminalCommit::NotOwned);
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
        transaction.commit()?;
        Ok(AdmittedTerminalCommit::Applied)
    }

    pub async fn record_model_response_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        protocol_turn_id: TurnId,
        response: ModelResponseWrite,
    ) -> Result<Vec<RunEvent>, StorageError> {
        let started_at_ms = SystemClock::now_ms();
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
        let mut events = Vec::with_capacity(response.tool_calls.len().saturating_add(1));
        if let Some(text) = response.assistant_text.filter(|text| !text.is_empty()) {
            let sequence_no = response
                .assistant_protocol_sequence_no
                .unwrap_or(next_fallback_sequence_no);
            next_fallback_sequence_no =
                next_fallback_sequence_no.max(sequence_no.saturating_add(1));
            let event = RunEvent::AssistantMessageCommitted {
                response_id: response.response_id,
                text,
            };
            insert_protocol_projection_if_requested(
                &transaction,
                &event,
                Some(session_id),
                protocol_turn_id,
                Some(sequence_no),
            )?;
            events.push(event);
        }
        for call in response.tool_calls {
            let sequence_no = call
                .protocol_sequence_no
                .unwrap_or(next_fallback_sequence_no);
            next_fallback_sequence_no =
                next_fallback_sequence_no.max(sequence_no.saturating_add(1));
            let event = RunEvent::ToolCallPending {
                tool_call_id: call.id,
                response_id: response.response_id,
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
        let base_url = ProviderEndpoint::parse(&draft.base_url)
            .map_err(|error| StorageError::Message(error.to_string()))?
            .as_str()
            .to_string();
        let id = SessionId::new();
        let now = SystemClock.now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO sessions (id, project_id, title, status, cwd_path, model_name, base_url, access_mode, model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '{}', ?9, ?10, NULL)",
            params![
                id.to_string(),
                draft.project_id.to_string(),
                draft.title,
                "idle",
                draft.cwd.as_str(),
                draft.model,
                base_url,
                draft.access_mode.as_str(),
                now,
                now
            ],
        )?;
        drop(connection);
        self.get_session(id).await
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
                active_session_for_mutation_branch(&transaction, id, true, now)?
        {
            return Err(StorageError::Message(format!(
                "session {id} has active agent-tree session {active_session_id}; stop the agent tree before archiving it"
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
            active_session_for_mutation_branch(&transaction, id, false, SystemClock::now_ms())?
        {
            return Err(StorageError::Message(format!(
                "session {active_session_id} is active; settings update requires an idle or terminal session"
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

fn active_session_for_mutation_branch(
    connection: &Connection,
    session_id: SessionId,
    include_direct_children: bool,
    now_ms: i64,
) -> Result<Option<SessionId>, StorageError> {
    let is_direct_child = connection
        .query_row(
            "SELECT 1 FROM session_spawn_edges WHERE child_session_id = ?1",
            params![session_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let include_direct_children = include_direct_children && !is_direct_child;
    let mut statement = connection.prepare(
        "SELECT session.id, session.status, session.active_run_id,
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
         WHERE (
                session.id = ?1
                OR (
                    ?2
                    AND session.id IN (
                        SELECT child_session_id
                        FROM session_spawn_edges
                        WHERE root_session_id = ?1
                    )
                )
               )
         ORDER BY CASE WHEN session.id = ?1 THEN 0 ELSE 1 END, session.id ASC",
    )?;
    let mut rows = statement.query(params![session_id.to_string(), include_direct_children])?;
    let mut first_blocker = None;
    while let Some(row) = rows.next()? {
        let candidate_session_id = parse_session_id_column(row, 0)?;
        let runtime_state = validate_raw_session_runtime_state(
            candidate_session_id,
            raw_session_runtime_state_from_row(row, 1)?,
        )?;
        if first_blocker.is_none() && runtime_state.blocks_mutation_at(now_ms) {
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
            final_response_id: snapshot.final_response_id,
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
    final_response_id: Option<ModelResponseId>,
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
    let mut final_response_id = None;
    let mut tool_calls = Vec::<(ToolCallId, crate::tool::ToolName)>::new();
    let mut settled_tool_calls = HashSet::<ToolCallId>::new();
    let mut failed_tool_count = 0usize;
    let mut change_count = 0usize;
    for payload_json in payloads {
        match serde_json::from_str::<HistoryItemPayload>(&payload_json)? {
            HistoryItemPayload::AssistantMessage { response_id, .. } => {
                final_response_id = Some(response_id);
            }
            HistoryItemPayload::ToolCall {
                call_id,
                response_id,
                tool_name,
                ..
            } => {
                final_response_id = Some(response_id);
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
        final_response_id,
        tool_call_count,
        failed_tool_count,
        change_count,
        unsettled_tool_calls,
    })
}

fn session_spawn_edge_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSpawnEdge> {
    let edge = SessionSpawnEdge {
        root_session_id: parse_session_id_column(row, 0)?,
        parent_session_id: parse_session_id_column(row, 1)?,
        child_session_id: parse_session_id_column(row, 2)?,
        agent_path: row.get(3)?,
        task_name: row.get(4)?,
        created_at_ms: row.get(5)?,
    };
    validate_flat_session_spawn_edge(
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

fn validate_flat_session_spawn_edge(
    root_session_id: SessionId,
    parent_session_id: SessionId,
    child_session_id: SessionId,
    agent_path: &str,
    task_name: &str,
) -> Result<(), String> {
    if parent_session_id != root_session_id {
        return Err(format!(
            "spawn parent session {parent_session_id} is not root session {root_session_id}; only root → direct-child lineage is supported"
        ));
    }
    if child_session_id == root_session_id {
        return Err(format!(
            "root session {root_session_id} cannot also be its own child session"
        ));
    }
    let expected_path = AgentPath::root()
        .join(task_name)
        .map_err(|error| format!("invalid direct-child task name `{task_name}`: {error}"))?;
    if expected_path.as_str() != agent_path {
        return Err(format!(
            "spawn edge path `{agent_path}` does not match canonical direct-child path `{expected_path}`"
        ));
    }
    Ok(())
}

fn delete_session_rows(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<(), StorageError> {
    let session_id = session_id.to_string();
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

fn count_steer_history_items(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<usize, StorageError> {
    count_history_items_by_kind(transaction, session_id, "steer_turn")
}

fn count_agent_communication_history_items(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<usize, StorageError> {
    count_history_items_by_kind(transaction, session_id, "inter_agent_communication")
}

fn count_history_items_by_kind(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    kind: &'static str,
) -> Result<usize, StorageError> {
    let count = transaction.query_row(
        "SELECT COUNT(*)
         FROM protocol_history_items
         WHERE session_id = ?1
           AND json_extract(payload_json, '$.kind') = ?2",
        params![session_id.to_string(), kind],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).map_err(|_| {
        StorageError::Message(format!(
            "history item count for kind `{kind}` exceeds this platform's range"
        ))
    })
}

pub(crate) fn normalize_run_lease_now_ms(now_ms: i64) -> i64 {
    now_ms.clamp(0, i64::MAX - 1)
}

fn run_lease_expiry_ms(now_ms: i64, lease_duration_ms: i64) -> i64 {
    normalize_run_lease_now_ms(now_ms).saturating_add(lease_duration_ms.max(1))
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
    validate_retained_admission_terminal_state_in_connection(
        transaction,
        session_id,
        ValidatedSessionRuntimeState {
            status: current_status,
            admission: Some(recovery_admission),
        },
    )?;
    if was_active {
        transaction.execute(
            "UPDATE sessions
             SET status = 'failed',
                 updated_at_ms = ?2,
                 completed_at_ms = ?2,
                 active_run_id = NULL,
                 active_turn_id = NULL,
                 active_run_lease_expires_at_ms = NULL
             WHERE id = ?1",
            params![session_id.to_string(), now_ms],
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
                outcome: TurnTerminalOutcome::Failed {
                    error: EXPIRED_RUN_RECOVERY_REASON.to_string(),
                },
                final_response_id: snapshot.final_response_id,
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
    }
    // A terminal session already settled the tools owned by this turn. Expiry only releases
    // the stale admission; it must not reclassify first-writer terminal outcomes.
    Ok(())
}

fn require_active_admission_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
) -> Result<(), StorageError> {
    let now = normalize_run_lease_now_ms(SystemClock::now_ms());
    let owned = session_runtime_state_from_connection(transaction, session_id)?
        .filter(|runtime_state| runtime_state.status == SessionStatus::Running)
        .and_then(|runtime_state| runtime_state.fresh_admission_at(now))
        .is_some_and(|admission| {
            admission.admission_id == admission_id && admission.turn_id == turn_id
        });
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
        let root = store
            .session_repo()
            .get_session(root_session_id)
            .await
            .expect("root session");
        store
            .session_repo()
            .create_session(NewSession {
                project_id: root.project_id,
                title: title.to_string(),
                cwd: root.cwd,
                model: root.model,
                base_url: root.base_url,
                access_mode: root.access_mode,
            })
            .await
            .expect("sibling session")
    }

    #[tokio::test]
    async fn spawn_edge_repository_accepts_only_flat_root_children() {
        let (store, root_session_id) = test_repo().await;
        let direct = create_sibling_session(&store, root_session_id, "direct").await;
        let rejected = create_sibling_session(&store, root_session_id, "rejected").await;
        let repository = store.session_repo();

        let inserted = repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                direct.id,
                "/root/direct",
                "direct",
            )
            .await
            .expect("direct edge");
        assert_eq!(inserted.parent_session_id, root_session_id);
        assert_eq!(inserted.agent_path, "/root/direct");

        let cases = [
            (
                direct.id,
                rejected.id,
                "/root/rejected",
                "rejected",
                "only root → direct-child lineage",
            ),
            (
                root_session_id,
                rejected.id,
                "/root/direct/rejected",
                "rejected",
                "does not match canonical direct-child path",
            ),
            (
                root_session_id,
                rejected.id,
                "/root/BadName",
                "BadName",
                "invalid direct-child task name",
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
                .expect("flat edges"),
            vec![inserted]
        );
        let initial_states = repository
            .list_direct_child_run_admission_states(root_session_id)
            .await
            .expect("initial direct child states");
        assert_eq!(initial_states.len(), 1);
        assert!(!initial_states[0].blocks_new_root_turn);
        repository
            .admit_session_turn(direct.id, TurnId::new())
            .await
            .expect("child admission")
            .expect("child admitted");
        let admitted_states = repository
            .list_direct_child_run_admission_states(root_session_id)
            .await
            .expect("admitted direct child states");
        assert_eq!(admitted_states.len(), 1);
        assert!(admitted_states[0].blocks_new_root_turn);
        assert!(
            repository.get_session(rejected.id).await.is_ok(),
            "a rejected edge must not delete the independent session"
        );
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
        let (_, turn_id) = active_turn(&store, session_id).await;
        let repository = store.session_repo();
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER abort_steer_history
                 BEFORE INSERT ON protocol_history_items
                 WHEN json_extract(NEW.payload_json, '$.kind') = 'steer_turn'
                 BEGIN SELECT RAISE(ABORT, 'injected steer history failure'); END;",
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
            .execute_batch("DROP TRIGGER abort_steer_history;")
            .expect("drop failure trigger");
        let committed_id = repository
            .accept_active_turn_steer(session_id, &steer)
            .await
            .expect("retry steer");
        let committed = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history after retry")
            .into_iter()
            .filter(|item| matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
            .collect::<Vec<_>>();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].id, committed_id);
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
    async fn concurrent_admission_commits_exactly_one_run_and_turn_owner() {
        let (store, session_id) = test_repo().await;
        let first_repository = store.session_repo();
        let second_repository = store.session_repo();
        let first_turn_id = TurnId::new();
        let second_turn_id = TurnId::new();
        let (first, second) = tokio::join!(
            first_repository.admit_session_turn(session_id, first_turn_id),
            second_repository.admit_session_turn(session_id, second_turn_id),
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
        let admission = repository
            .admit_session_turn_with_goal_objective(session_id, turn_id, "admitted objective")
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
        let admitted = match repository
            .admit_active_goal_continuation_turn(session_id, admitted_turn_id)
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
        assert_eq!(terminal.final_response_id, Some(response_id));
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
        assert_eq!(terminal.final_response_id, Some(response_id));
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
        assert_eq!(terminal.final_response_id, Some(response_id));
        assert_eq!(terminal.tool_call_count, 1);
        assert_eq!(terminal.failed_tool_count, 0);
        assert_eq!(terminal.change_count, 2);
    }

    #[tokio::test]
    async fn terminal_cas_observes_committed_agent_mail_and_active_append_loses_to_terminal() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root/worker".to_string(),
                    recipient: "/root".to_string(),
                    content: "new evidence".to_string(),
                    trigger_turn: false,
                },
                true,
            )
            .expect("active mail append");
        let active_mail = store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("active turn history")
            .into_iter()
            .find(|item| {
                matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                )
            })
            .expect("active mail history");
        assert_eq!(
            active_mail.scope,
            crate::protocol::HistoryScope::Turn { turn_id }
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
                    Some(0),
                    Some(0),
                    None,
                )
                .await
                .expect("terminal CAS"),
            AdmittedTerminalCommit::UnseenAgentCommunication {
                expected: 0,
                actual: 1,
            }
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &terminal,
                    turn_id,
                    None,
                    Some(0),
                    Some(1),
                    None,
                )
                .await
                .expect("terminal retry"),
            AdmittedTerminalCommit::Applied
        );
        let append_after_terminal = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root/worker".to_string(),
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
    async fn communication_for_an_inactive_recipient_is_session_scoped() {
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
                    Some(0),
                    Some(0),
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
                    author: "/root/worker".to_string(),
                    recipient: "/root".to_string(),
                    content: "evidence for a future continuation".to_string(),
                    trigger_turn: false,
                },
                false,
            )
            .expect("inactive recipient communication");
        let communication = store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history")
            .into_iter()
            .find(|item| item.id == communication_id)
            .expect("communication item");

        assert_eq!(communication.scope, crate::protocol::HistoryScope::Session);
        assert_eq!(communication.turn_id(), None);
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
                    Some(0),
                    Some(0),
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
                    author: "/root/worker-a".to_string(),
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
                    author: "/root/worker-b".to_string(),
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
        let after = store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history after rollback");

        assert_eq!(rolled_back.dropped_turn_ids, vec![completed_turn_id]);
        assert!(after.iter().any(|item| item.id == first_mail_id));
        assert!(after.iter().any(|item| item.id == second_mail_id));
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
