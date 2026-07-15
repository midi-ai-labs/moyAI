use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::config::AccessMode;
use crate::error::StorageError;
use crate::protocol::{
    HistoryItem, HistoryItemId, HistoryItemPayload, InterAgentCommunication, ModelResponseId,
    RuntimeEvent, RuntimeEventId, RuntimeEventMsg, SteerTurn, TurnId, TurnItem, TurnItemId,
    TurnItemPayload, UserTurn, fork_canonical_items_in_transaction,
    insert_session_owned_event_bundle_in_transaction, latest_protocol_turn_ids_in_transaction,
    latest_turn_position_for_session, project_inter_agent_communication,
    project_protocol_run_event,
};
use crate::runtime::{Clock, SystemClock};
use crate::session::{
    FinishReason, NewSession, ProjectId, RunEvent, SessionForkResult, SessionId,
    SessionModelParameters, SessionRecord, SessionRepository, SessionSettingsPatch,
    SessionSettingsUpdate, SessionSpawnEdge, SessionStatus, SessionTitleUpdate, ThreadGoal,
    ThreadGoalStatus, ToolCallId, ToolCallStatus, validate_thread_goal_objective,
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
pub enum AdmittedTerminalCommit {
    Applied,
    AlreadyTerminalizedBySameAdmission,
    UnseenSteer { expected: usize, actual: usize },
    UnseenAgentCommunication { expected: usize, actual: usize },
    NotOwned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAdmissionLeaseRenewalOutcome {
    Renewed,
    GracefulTerminal,
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

    pub async fn list_direct_child_spawn_edges(
        &self,
        parent_session_id: SessionId,
    ) -> Result<Vec<SessionSpawnEdge>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT root_session_id, parent_session_id, child_session_id,
                    agent_path, task_name, created_at_ms
             FROM session_spawn_edges
             WHERE parent_session_id = ?1
             ORDER BY created_at_ms ASC, child_session_id ASC",
        )?;
        statement
            .query_map(
                params![parent_session_id.to_string()],
                session_spawn_edge_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
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

    pub async fn list_running_sessions_for_recovery(
        &self,
    ) -> Result<Vec<SessionRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id FROM sessions
             WHERE status = 'running'
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        let mut sessions = Vec::with_capacity(ids.len());
        for value in ids {
            sessions.push(
                self.get_session(
                    value
                        .parse::<SessionId>()
                        .map_err(|error| StorageError::Message(error.to_string()))?,
                )
                .await?,
            );
        }
        Ok(sessions)
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
        let mut statement = transaction.prepare(
            "SELECT parent_session_id, child_session_id
             FROM session_spawn_edges
             ORDER BY created_at_ms ASC, child_session_id ASC",
        )?;
        let relationships = statement
            .query_map([], |row| {
                Ok((
                    parse_session_id_column(row, 0)?,
                    parse_session_id_column(row, 1)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        let mut children = HashMap::<SessionId, Vec<SessionId>>::new();
        for (parent_session_id, child_session_id) in relationships {
            children
                .entry(parent_session_id)
                .or_default()
                .push(child_session_id);
        }
        let mut deleted_session_ids = Vec::new();
        collect_session_tree_postorder(
            session_id,
            &children,
            &mut HashSet::new(),
            &mut deleted_session_ids,
        );

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

    pub async fn list_sessions_with_projection_state(
        &self,
        project_id: ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<(SessionRecord, bool)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    archived_at_ms IS NOT NULL
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
            .query_map(params![project_id.to_string(), limit as i64], |row| {
                let id = row
                    .get::<_, String>(0)?
                    .parse::<SessionId>()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                Ok((
                    SessionRecord {
                        id,
                        project_id,
                        title: row.get(1)?,
                        status: parse_status_column(row, 2)?,
                        cwd: row.get::<_, String>(3)?.into(),
                        model: row.get(4)?,
                        base_url: row.get(5)?,
                        access_mode: parse_access_mode(&row.get::<_, String>(6)?),
                        model_parameters: parse_session_model_parameters(
                            &row.get::<_, String>(7)?,
                            7,
                        )?,
                        created_at_ms: row.get(8)?,
                        updated_at_ms: row.get(9)?,
                        completed_at_ms: row.get(10)?,
                    },
                    row.get::<_, bool>(11)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub async fn search_sessions_with_projection_state(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<(SessionRecord, bool)>, StorageError> {
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
            "SELECT id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    archived_at_ms IS NOT NULL
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
                params![project_id.to_string(), normalized, limit as i64],
                |row| {
                    let id = row
                        .get::<_, String>(0)?
                        .parse::<SessionId>()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    Ok((
                        SessionRecord {
                            id,
                            project_id,
                            title: row.get(1)?,
                            status: parse_status_column(row, 2)?,
                            cwd: row.get::<_, String>(3)?.into(),
                            model: row.get(4)?,
                            base_url: row.get(5)?,
                            access_mode: parse_access_mode(&row.get::<_, String>(6)?),
                            model_parameters: parse_session_model_parameters(
                                &row.get::<_, String>(7)?,
                                7,
                            )?,
                            created_at_ms: row.get(8)?,
                            updated_at_ms: row.get(9)?,
                            completed_at_ms: row.get(10)?,
                        },
                        row.get::<_, bool>(11)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
        let active_tree_session = transaction
            .query_row(
                "SELECT id
                 FROM sessions
                 WHERE (
                     id = ?1
                     OR id IN (
                         SELECT child_session_id
                         FROM session_spawn_edges
                         WHERE root_session_id = ?1
                     )
                 )
                   AND (status = 'running' OR active_run_id IS NOT NULL)
                 ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END, id ASC
                 LIMIT 1",
                params![root_session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
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
        let source_active_turn_id = transaction
            .query_row(
                "SELECT active_turn_id FROM sessions WHERE id = ?1",
                params![source_session_id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )?
            .map(|value| {
                value
                    .parse::<TurnId>()
                    .map_err(|error| StorageError::Message(error.to_string()))
            })
            .transpose()?;
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
            let snapshot_turn_id = match source_active_turn_id {
                Some(turn_id) => turn_id,
                None => latest_turn_position_for_session(&transaction, target_session_id)?
                    .map(|(turn_id, _)| turn_id)
                    .unwrap_or_else(TurnId::new),
            };
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

    pub async fn append_user_turn_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: &str,
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

    pub fn append_inter_agent_communication_with_protocol_bundle(
        &self,
        session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
    ) -> Result<HistoryItemId, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = transaction
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
            .optional()?;
        let Some((status, active_run_id, active_turn_id, lease_expires_at_ms)) = state else {
            return Err(StorageError::Message(format!(
                "inter-agent communication target session {session_id} does not exist"
            )));
        };
        let active_turn_id = if status == "running"
            && active_run_id.is_some()
            && run_lease_is_fresh(lease_expires_at_ms, now)
        {
            active_turn_id
                .map(|value| {
                    value
                        .parse::<TurnId>()
                        .map_err(|error| StorageError::Message(error.to_string()))
                })
                .transpose()?
        } else {
            None
        };
        let has_active_admission = active_turn_id.is_some();
        if require_active_recipient && !has_active_admission {
            return Err(StorageError::Message(format!(
                "recipient session {session_id} became terminal before inter-agent communication could be committed"
            )));
        }
        let turn_id = match active_turn_id {
            Some(turn_id) => turn_id,
            None => TurnId::new(),
        };
        let projection = project_inter_agent_communication(session_id, turn_id, 0, communication);
        let stored = insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            &transaction,
            &projection.runtime_event,
            projection.history_item.as_ref(),
            projection.turn_item.as_ref(),
        )?;
        let history_item_id = stored
            .history_item
            .ok_or_else(|| {
                StorageError::Message(
                    "inter-agent communication projection omitted canonical history".to_string(),
                )
            })?
            .id;
        transaction.commit()?;
        Ok(history_item_id)
    }

    #[cfg(test)]
    pub(crate) async fn set_status_for_test(
        &self,
        session_id: SessionId,
        status: SessionStatus,
    ) -> Result<(), StorageError> {
        if status_is_terminal(status) {
            return Err(StorageError::Message(
                "terminal session status must be written by a TurnTerminal event".to_string(),
            ));
        }
        let now = SystemClock.now_ms();
        let status_text = match status {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Completed => "completed",
            SessionStatus::Cancelled => "cancelled",
            SessionStatus::Failed => "failed",
        };
        let completed_at_ms: Option<i64> = None;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE sessions
             SET status = ?2,
                 updated_at_ms = ?3,
                 completed_at_ms = ?4
             WHERE id = ?1",
            params![session_id.to_string(), status_text, now, completed_at_ms],
        )?;
        Ok(())
    }

    pub async fn admit_session_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<String>, StorageError> {
        self.admit_session_turn_at(
            session_id,
            turn_id,
            SystemClock::now_ms(),
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
    }

    pub async fn admit_session_turn_at(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<Option<String>, StorageError> {
        let admission_id = ulid::Ulid::new().to_string();
        let now = normalize_run_lease_now_ms(now_ms);
        let lease_expires_at_ms = run_lease_expiry_ms(now, lease_duration_ms);
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT status, active_run_id, active_turn_id,
                        active_run_lease_expires_at_ms
                 FROM sessions
                 WHERE id = ?1",
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
            .optional()?;
        let Some((status, active_run_id, active_turn_id, lease_expires_at_ms_current)) = current
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if active_run_id.is_some() {
            if run_lease_is_fresh(lease_expires_at_ms_current, now) {
                transaction.commit()?;
                return Ok(None);
            }
            recover_expired_run_admission_in_transaction(
                &transaction,
                session_id,
                &status,
                active_turn_id.as_deref(),
                now,
            )?;
        } else if status == "running" {
            recover_expired_run_admission_in_transaction(
                &transaction,
                session_id,
                &status,
                active_turn_id.as_deref(),
                now,
            )?;
        }
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
                admission_id,
                turn_id.to_string(),
                lease_expires_at_ms
            ],
        )? == 1;
        transaction.commit()?;
        Ok(admitted.then_some(admission_id))
    }

    pub async fn renew_admitted_run_lease(
        &self,
        session_id: SessionId,
        admission_id: &str,
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
        admission_id: &str,
        turn_id: TurnId,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<RunAdmissionLeaseRenewalOutcome, StorageError> {
        let now = normalize_run_lease_now_ms(now_ms);
        let requested_expiry = run_lease_expiry_ms(now, lease_duration_ms);
        let turn_id_text = turn_id.to_string();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = transaction
            .query_row(
                "SELECT status, active_run_id, active_turn_id,
                        active_run_lease_expires_at_ms
                 FROM sessions
                 WHERE id = ?1",
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
            .optional()?;
        let outcome = match state {
            Some((status, active_run_id, active_turn_id, lease_expires_at_ms))
                if active_run_id.as_deref() == Some(admission_id)
                    && active_turn_id.as_deref() == Some(turn_id_text.as_str())
                    && run_lease_is_fresh(lease_expires_at_ms, now)
                    && status == "running" =>
            {
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
                        admission_id,
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
            Some((status, active_run_id, active_turn_id, lease_expires_at_ms))
                if matches!(status.as_str(), "completed" | "cancelled" | "failed")
                    && ((active_run_id.as_deref() == Some(admission_id)
                        && active_turn_id.as_deref() == Some(turn_id_text.as_str())
                        && run_lease_is_fresh(lease_expires_at_ms, now))
                        || active_run_id.is_none()) =>
            {
                RunAdmissionLeaseRenewalOutcome::GracefulTerminal
            }
            _ => RunAdmissionLeaseRenewalOutcome::SupersededOrExpired,
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub async fn admitted_run_status(
        &self,
        session_id: SessionId,
        admission_id: &str,
        turn_id: TurnId,
    ) -> Result<Option<SessionStatus>, StorageError> {
        self.admitted_run_status_at(session_id, admission_id, turn_id, SystemClock::now_ms())
            .await
    }

    pub async fn admitted_run_status_at(
        &self,
        session_id: SessionId,
        admission_id: &str,
        turn_id: TurnId,
        now_ms: i64,
    ) -> Result<Option<SessionStatus>, StorageError> {
        let now = normalize_run_lease_now_ms(now_ms);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let status = connection
            .query_row(
                "SELECT status
                 FROM sessions
                 WHERE id = ?1
                   AND active_run_id = ?2
                   AND active_turn_id = ?3
                   AND active_run_lease_expires_at_ms > ?4",
                params![
                    session_id.to_string(),
                    admission_id,
                    turn_id.to_string(),
                    now
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|status| parse_status(&status))
            .transpose()?;
        Ok(status)
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
        let mut statement = transaction.prepare(
            "SELECT msg_json
             FROM protocol_runtime_events
             WHERE session_id = ?1 AND turn_id = ?2
             ORDER BY sequence_no DESC, rowid DESC",
        )?;
        let rows = statement.query_map(
            params![session_id.to_string(), turn_id.to_string()],
            |row| row.get::<_, String>(0),
        )?;
        let mut protocol_terminal = None;
        for row in rows {
            let msg_json = row?;
            let msg = serde_json::from_str::<RuntimeEventMsg>(&msg_json)?;
            if let RuntimeEventMsg::TurnTerminal { terminal } = msg {
                protocol_terminal = Some(*terminal);
                break;
            }
        }
        drop(statement);
        transaction.commit()?;
        Ok(protocol_terminal)
    }

    pub async fn active_turn_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<TurnId>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let turn_id = connection
            .query_row(
                "SELECT active_turn_id
                 FROM sessions
                 WHERE id = ?1
                   AND active_run_id IS NOT NULL
                   AND active_turn_id IS NOT NULL
                   AND active_run_lease_expires_at_ms > ?2
                    AND status = 'running'",
                params![session_id.to_string(), now],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| {
                value
                    .parse::<TurnId>()
                    .map_err(|error| StorageError::Message(error.to_string()))
            })
            .transpose()?;
        Ok(turn_id)
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
        let fresh = connection
            .query_row(
                "SELECT 1
                 FROM sessions
                 WHERE id = ?1
                   AND active_run_id IS NOT NULL
                   AND active_run_lease_expires_at_ms > ?2",
                params![session_id.to_string(), now],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        Ok(fresh)
    }

    pub async fn list_admitted_turn_steers(
        &self,
        session_id: SessionId,
        admission_id: &str,
        turn_id: TurnId,
    ) -> Result<Option<Vec<HistoryItem>>, StorageError> {
        self.list_admitted_turn_steers_at(session_id, admission_id, turn_id, SystemClock::now_ms())
            .await
    }

    pub async fn list_admitted_turn_steers_at(
        &self,
        session_id: SessionId,
        admission_id: &str,
        turn_id: TurnId,
        now_ms: i64,
    ) -> Result<Option<Vec<HistoryItem>>, StorageError> {
        let now = normalize_run_lease_now_ms(now_ms);
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned = transaction
            .query_row(
                "SELECT 1
                 FROM sessions
                 WHERE id = ?1
                   AND active_run_id = ?2
                   AND active_turn_id = ?3
                   AND active_run_lease_expires_at_ms > ?4
                    AND status = 'running'",
                params![
                    session_id.to_string(),
                    admission_id,
                    turn_id.to_string(),
                    now
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !owned {
            transaction.commit()?;
            return Ok(None);
        }
        let mut statement = transaction.prepare(
            "SELECT id, sequence_no, payload_json, created_at_ms
             FROM protocol_history_items
             WHERE session_id = ?1 AND turn_id = ?2
             ORDER BY sequence_no ASC",
        )?;
        let rows = statement.query_map(
            params![session_id.to_string(), turn_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut steers = Vec::new();
        for row in rows {
            let (id, sequence_no, payload_json, created_at_ms) = row?;
            let payload = serde_json::from_str::<HistoryItemPayload>(&payload_json)?;
            if matches!(payload, HistoryItemPayload::SteerTurn { .. }) {
                steers.push(HistoryItem {
                    id: id.parse::<HistoryItemId>().map_err(|error| {
                        StorageError::Message(format!("invalid steer history id: {error}"))
                    })?,
                    session_id,
                    turn_id,
                    sequence_no,
                    created_at_ms,
                    payload,
                });
            }
        }
        drop(statement);
        transaction.commit()?;
        Ok(Some(steers))
    }

    pub async fn release_stopped_run_admission(
        &self,
        session_id: SessionId,
        admission_id: &str,
    ) -> Result<bool, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let released = connection.execute(
            "UPDATE sessions
             SET active_run_id = NULL,
                 active_turn_id = NULL,
                 active_run_lease_expires_at_ms = NULL
             WHERE id = ?1
               AND active_run_id = ?2
               AND status != 'running'",
            params![session_id.to_string(), admission_id],
        )? == 1;
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
        let state = transaction
            .query_row(
                "SELECT status, active_run_id, active_turn_id,
                        active_run_lease_expires_at_ms
                 FROM sessions
                 WHERE id = ?1",
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
            .optional()?
            .ok_or_else(|| StorageError::Message(format!("session {session_id} was not found")))?;
        let (status, active_run_id, active_turn_id, lease_expires_at_ms) = state;
        if status != "running" {
            return Err(StorageError::Message(format!(
                "no active running turn to steer for session {session_id}; current status is {status}"
            )));
        }
        if active_run_id.is_none() {
            return Err(StorageError::Message(format!(
                "running session {session_id} has no durable run admission owner"
            )));
        }
        if !run_lease_is_fresh(lease_expires_at_ms, now) {
            return Err(StorageError::Message(format!(
                "run admission lease expired for session {session_id}"
            )));
        }
        let active_turn_id = active_turn_id
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "running session {session_id} has not published its active turn yet"
                ))
            })?
            .parse::<TurnId>()
            .map_err(|error| StorageError::Message(error.to_string()))?;
        if active_turn_id != steer.expected_turn_id {
            return Err(StorageError::Message(format!(
                "expected active turn id `{}` but current active turn id is `{active_turn_id}`",
                steer.expected_turn_id
            )));
        }

        let history_item = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: active_turn_id,
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
        let session_id = connection
            .query_row(
                "SELECT id FROM sessions
                 WHERE project_id = ?1
                   AND status = 'running'
                   AND active_run_lease_expires_at_ms > ?2
                 ORDER BY updated_at_ms DESC, id DESC
                 LIMIT 1",
                params![project_id.to_string(), now],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| {
                value
                    .parse::<SessionId>()
                    .map_err(|error| StorageError::Message(error.to_string()))
            })
            .transpose()?;
        Ok(session_id)
    }

    pub async fn terminalize_active_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<bool, StorageError> {
        Ok(self
            .terminalize_turn_with_protocol_event_guarded(
                session_id,
                event,
                protocol_turn_id,
                protocol_sequence_no,
                None,
                true,
                None,
                None,
                None,
            )
            .await?
            .was_applied())
    }

    pub async fn recover_orphaned_active_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<bool, StorageError> {
        Ok(self
            .terminalize_turn_with_protocol_event_guarded(
                session_id,
                event,
                protocol_turn_id,
                protocol_sequence_no,
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
        admission_id: &str,
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
            protocol_turn_id,
            protocol_sequence_no,
            Some(admission_id),
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
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        admission_id: Option<&str>,
        retain_active_admission: bool,
        expected_seen_steer_count: Option<usize>,
        expected_seen_agent_communication_count: Option<usize>,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        let terminal = validate_terminal_event(session_id, event)?;
        let status = terminal.status.as_session_status();
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let protocol_turn_id_text = protocol_turn_id.to_string();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let state = transaction
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
            .optional()?;
        let Some((current_status, active_run_id, active_turn_id, lease_expires_at_ms)) = state
        else {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::NotOwned);
        };

        if let Some(admission_id) = admission_id {
            if active_run_id.as_deref() != Some(admission_id)
                || active_turn_id.as_deref() != Some(protocol_turn_id_text.as_str())
                || !run_lease_is_fresh(lease_expires_at_ms, now)
            {
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::NotOwned);
            }
            if current_status != "running" {
                let already_terminal =
                    terminal_for_turn_in_transaction(&transaction, session_id, protocol_turn_id)?
                        .is_some();
                if already_terminal {
                    transaction.execute(
                        "UPDATE sessions
                         SET active_run_id = NULL,
                             active_turn_id = NULL,
                             active_run_lease_expires_at_ms = NULL
                         WHERE id = ?1 AND active_run_id = ?2 AND active_turn_id = ?3",
                        params![
                            session_id.to_string(),
                            admission_id,
                            protocol_turn_id.to_string(),
                        ],
                    )?;
                    transaction.commit()?;
                    return Ok(AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission);
                }
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::NotOwned);
            }
        } else {
            if active_turn_id
                .as_deref()
                .is_some_and(|active_turn_id| active_turn_id != protocol_turn_id.to_string())
                || current_status != "running"
            {
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::NotOwned);
            }
        }

        if terminal_for_turn_in_transaction(&transaction, session_id, protocol_turn_id)?.is_some() {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission);
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
                 SET status = ?4,
                     updated_at_ms = ?5,
                     completed_at_ms = ?5,
                     active_run_id = NULL,
                     active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1
                   AND (?2 IS NULL OR active_run_id = ?2)
                   AND (active_turn_id IS NULL OR active_turn_id = ?3)
                   AND status = 'running'",
                params![
                    session_id.to_string(),
                    admission_id,
                    protocol_turn_id.to_string(),
                    status_text,
                    now,
                ],
            )? == 1
        } else {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?4, updated_at_ms = ?5, completed_at_ms = ?5
                 WHERE id = ?1
                   AND (?2 IS NULL OR active_run_id = ?2)
                   AND (active_turn_id IS NULL OR active_turn_id = ?3)
                   AND status = 'running'",
                params![
                    session_id.to_string(),
                    admission_id,
                    protocol_turn_id.to_string(),
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
        admission_id: &str,
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
        admission_id: &str,
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
        admission_id: &str,
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
        admission_id: &str,
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
        admission_id: &str,
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
        admission_id: &str,
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
        admission_id: &str,
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
                draft.base_url,
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
        let id: Option<String> = connection
            .query_row(
                "SELECT id FROM sessions
                 WHERE project_id = ?1 AND archived_at_ms IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM session_spawn_edges
                       WHERE child_session_id = sessions.id
                   )
                 ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
                 LIMIT 1",
                params![project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        drop(connection);
        match id {
            Some(value) => self
                .get_session(
                    value
                        .parse::<SessionId>()
                        .map_err(|error| StorageError::Message(error.to_string()))?,
                )
                .await
                .map(Some),
            None => Ok(None),
        }
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
            "SELECT id FROM sessions
             WHERE project_id = ?1{archived_filter}
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?2"
        );
        let mut statement = connection.prepare(&sql)?;
        let ids = statement
            .query_map(params![project_id.to_string(), limit as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        let mut sessions = Vec::new();
        for value in ids {
            sessions.push(
                self.get_session(
                    value
                        .parse::<SessionId>()
                        .map_err(|error| StorageError::Message(error.to_string()))?,
                )
                .await?,
            );
        }
        Ok(sessions)
    }

    async fn list_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id FROM sessions
             WHERE archived_at_ms IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?1",
        )?;
        let ids = statement
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        let mut sessions = Vec::new();
        for value in ids {
            sessions.push(
                self.get_session(
                    value
                        .parse::<SessionId>()
                        .map_err(|error| StorageError::Message(error.to_string()))?,
                )
                .await?,
            );
        }
        Ok(sessions)
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
            "SELECT id FROM sessions
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
        let ids = statement
            .query_map(
                params![project_id.to_string(), normalized, limit as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        let mut sessions = Vec::new();
        for value in ids {
            sessions.push(
                self.get_session(
                    value
                        .parse::<SessionId>()
                        .map_err(|error| StorageError::Message(error.to_string()))?,
                )
                .await?,
            );
        }
        Ok(sessions)
    }

    async fn set_session_archived(
        &self,
        id: SessionId,
        archived: bool,
    ) -> Result<SessionRecord, StorageError> {
        let now = SystemClock::now_ms();
        let archived_at_ms = archived.then_some(now);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let changed = if archived {
            connection.execute(
                "UPDATE sessions
                 SET archived_at_ms = ?2, updated_at_ms = ?3
                 WHERE id = ?1 AND status != 'running'",
                params![id.to_string(), archived_at_ms, now],
            )?
        } else {
            connection.execute(
                "UPDATE sessions SET archived_at_ms = NULL, updated_at_ms = ?2 WHERE id = ?1",
                params![id.to_string(), now],
            )?
        };
        drop(connection);
        if changed == 0 && archived {
            let current = self.get_session(id).await?;
            if current.status == SessionStatus::Running {
                return Err(StorageError::Message(format!(
                    "session {} is active; stop it before archiving it",
                    current.id
                )));
            }
        }
        self.get_session(id).await
    }

    async fn update_session_settings(
        &self,
        id: SessionId,
        patch: &SessionSettingsPatch,
    ) -> Result<SessionSettingsUpdate, StorageError> {
        for _ in 0..8 {
            let current = self.get_session(id).await?;
            let next_cwd = patch.cwd.clone().unwrap_or_else(|| current.cwd.clone());
            let next_model = patch.model.clone().unwrap_or_else(|| current.model.clone());
            let next_base_url = patch
                .base_url
                .clone()
                .unwrap_or_else(|| current.base_url.clone());
            let next_access_mode = patch.access_mode.unwrap_or(current.access_mode);
            let next_model_parameters = patch.apply_to_model_parameters(&current.model_parameters);
            let changed = next_cwd != current.cwd
                || next_model != current.model
                || next_base_url != current.base_url
                || next_access_mode != current.access_mode
                || next_model_parameters != current.model_parameters;
            if !changed {
                return Ok(SessionSettingsUpdate {
                    session: current,
                    changed: false,
                });
            }
            if current.status == SessionStatus::Running {
                return Err(StorageError::Message(format!(
                    "session {} is {}; settings update requires an idle or terminal session",
                    current.id,
                    current.status.key()
                )));
            }
            let now = SystemClock::now_ms().max(current.updated_at_ms.saturating_add(1));
            let connection = self.connection.lock().expect("sqlite mutex poisoned");
            let updated = connection.execute(
                "UPDATE sessions
                 SET cwd_path = ?2, model_name = ?3, base_url = ?4, access_mode = ?5,
                     model_parameters_json = ?6, updated_at_ms = ?7
                 WHERE id = ?1
                   AND updated_at_ms = ?8
                   AND status != 'running'",
                params![
                    id.to_string(),
                    next_cwd.as_str(),
                    next_model,
                    next_base_url,
                    next_access_mode.as_str(),
                    serde_json::to_string(&next_model_parameters)?,
                    now,
                    current.updated_at_ms
                ],
            )?;
            drop(connection);
            if updated == 1 {
                return Ok(SessionSettingsUpdate {
                    session: self.get_session(id).await?,
                    changed: true,
                });
            }
        }
        Err(StorageError::Message(
            "session settings changed repeatedly while applying an update; retry the operation"
                .to_string(),
        ))
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

fn session_record_from_connection(
    connection: &Connection,
    id: SessionId,
) -> Result<SessionRecord, StorageError> {
    connection
        .query_row(
            "SELECT project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms
             FROM sessions WHERE id = ?1",
            params![id.to_string()],
            |row| {
                Ok(SessionRecord {
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
                    base_url: row.get(5)?,
                    access_mode: parse_access_mode(&row.get::<_, String>(6)?),
                    model_parameters: parse_session_model_parameters(&row.get::<_, String>(7)?, 7)?,
                    created_at_ms: row.get(8)?,
                    updated_at_ms: row.get(9)?,
                    completed_at_ms: row.get(10)?,
                })
            },
        )
        .map_err(StorageError::from)
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
            status: crate::protocol::TurnTerminalStatus::Interrupted,
            finish_reason: Some(FinishReason::Cancelled),
            interruption_cause: Some(crate::protocol::TurnInterruptionCause::AgentInterrupted),
            final_response_id: snapshot.final_response_id,
            summary: reason.to_string(),
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
    Ok(SessionSpawnEdge {
        root_session_id: parse_session_id_column(row, 0)?,
        parent_session_id: parse_session_id_column(row, 1)?,
        child_session_id: parse_session_id_column(row, 2)?,
        agent_path: row.get(3)?,
        task_name: row.get(4)?,
        created_at_ms: row.get(5)?,
    })
}

fn collect_session_tree_postorder(
    session_id: SessionId,
    children: &HashMap<SessionId, Vec<SessionId>>,
    visited: &mut HashSet<SessionId>,
    result: &mut Vec<SessionId>,
) {
    if !visited.insert(session_id) {
        return;
    }
    if let Some(child_session_ids) = children.get(&session_id) {
        for child_session_id in child_session_ids {
            collect_session_tree_postorder(*child_session_id, children, visited, result);
        }
    }
    result.push(session_id);
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

#[cfg(test)]
fn status_is_terminal(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
    )
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
    let valid_shape = match terminal.status {
        crate::protocol::TurnTerminalStatus::Completed => {
            terminal.interruption_cause.is_none()
                && matches!(terminal.finish_reason, None | Some(FinishReason::Stop))
        }
        crate::protocol::TurnTerminalStatus::Interrupted => {
            terminal.interruption_cause.is_some()
                && terminal.finish_reason == Some(FinishReason::Cancelled)
        }
        crate::protocol::TurnTerminalStatus::Failed => {
            terminal.interruption_cause.is_none()
                && terminal.finish_reason == Some(FinishReason::Error)
        }
    };
    if !valid_shape {
        return Err(StorageError::Message(format!(
            "TurnTerminal fields contradict status {:?}",
            terminal.status
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

fn terminal_for_turn_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<crate::session::model::DurableTurnTerminal>, StorageError> {
    let mut statement = transaction.prepare(
        "SELECT msg_json
         FROM protocol_runtime_events
         WHERE session_id = ?1 AND turn_id = ?2
         ORDER BY sequence_no DESC, rowid DESC",
    )?;
    let rows = statement.query_map(
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    for row in rows {
        let msg = serde_json::from_str::<RuntimeEventMsg>(&row?)?;
        if let RuntimeEventMsg::TurnTerminal { terminal } = msg {
            return Ok(Some(*terminal));
        }
    }
    Ok(None)
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

fn parse_access_mode(value: &str) -> AccessMode {
    AccessMode::parse(value).unwrap_or(AccessMode::Default)
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
        turn_id,
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

fn count_steer_history_items(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<usize, StorageError> {
    let mut statement = transaction
        .prepare("SELECT payload_json FROM protocol_history_items WHERE session_id = ?1")?;
    let rows = statement.query_map(params![session_id.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    let mut count = 0;
    for row in rows {
        let payload = serde_json::from_str::<HistoryItemPayload>(&row?)?;
        if matches!(payload, HistoryItemPayload::SteerTurn { .. }) {
            count += 1;
        }
    }
    Ok(count)
}

fn count_agent_communication_history_items(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<usize, StorageError> {
    let mut statement = transaction
        .prepare("SELECT payload_json FROM protocol_history_items WHERE session_id = ?1")?;
    let rows = statement.query_map(params![session_id.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    let mut count = 0;
    for row in rows {
        let payload = serde_json::from_str::<HistoryItemPayload>(&row?)?;
        if matches!(payload, HistoryItemPayload::InterAgentCommunication { .. }) {
            count += 1;
        }
    }
    Ok(count)
}

pub(crate) fn normalize_run_lease_now_ms(now_ms: i64) -> i64 {
    now_ms.clamp(0, i64::MAX - 1)
}

fn run_lease_expiry_ms(now_ms: i64, lease_duration_ms: i64) -> i64 {
    normalize_run_lease_now_ms(now_ms).saturating_add(lease_duration_ms.max(1))
}

fn run_lease_is_fresh(lease_expires_at_ms: Option<i64>, now_ms: i64) -> bool {
    lease_expires_at_ms
        .is_some_and(|expires_at_ms| expires_at_ms > normalize_run_lease_now_ms(now_ms))
}

fn recover_expired_run_admission_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    current_status: &str,
    active_turn_id: Option<&str>,
    now_ms: i64,
) -> Result<(), StorageError> {
    let was_active = current_status == "running";
    let active_turn_id = active_turn_id
        .map(|value| {
            value
                .parse::<TurnId>()
                .map_err(|error| StorageError::Message(error.to_string()))
        })
        .transpose()?;
    let recovery_turn_id = active_turn_id;
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
    if let Some(turn_id) = recovery_turn_id {
        if was_active {
            let snapshot =
                canonical_turn_snapshot_in_transaction(transaction, session_id, turn_id)?;
            let recoverable_unfinished_count = count_unfinished_tool_calls_for_turn_in_transaction(
                transaction,
                session_id,
                turn_id,
            )?;
            let event = RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(crate::session::model::DurableTurnTerminal {
                    status: crate::protocol::TurnTerminalStatus::Failed,
                    finish_reason: Some(FinishReason::Error),
                    interruption_cause: None,
                    final_response_id: snapshot.final_response_id,
                    summary: EXPIRED_RUN_RECOVERY_REASON.to_string(),
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
        // the stale admission; it must not reclassify first-writer terminal outcomes or unrelated
        // DB-only rows that have no exact protocol owner.
    } else {
        // Legacy admissions without a turn owner cannot safely attribute their unfinished tools
        // to a canonical turn. Settle their rows only; current-turn recovery above records the
        // complete tool and terminal projection bundle.
        fail_unfinished_tool_calls_in_transaction(transaction, session_id, now_ms)?;
    }
    Ok(())
}

fn require_active_admission_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: &str,
    turn_id: TurnId,
) -> Result<(), StorageError> {
    let now = normalize_run_lease_now_ms(SystemClock::now_ms());
    let owned = transaction
        .query_row(
            "SELECT 1
             FROM sessions
             WHERE id = ?1
               AND active_run_id = ?2
               AND active_turn_id = ?3
               AND active_run_lease_expires_at_ms > ?4
               AND status = 'running'",
            params![
                session_id.to_string(),
                admission_id,
                turn_id.to_string(),
                now
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if owned {
        Ok(())
    } else {
        Err(StorageError::Message(format!(
            "run admission {admission_id} no longer owns active turn {turn_id} for session {session_id}"
        )))
    }
}

fn fail_unfinished_tool_calls_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    finished_at_ms: i64,
) -> Result<(), StorageError> {
    transaction.execute(
        "UPDATE tool_calls
         SET status = 'failed',
             finished_at_ms = COALESCE(finished_at_ms, ?2)
         WHERE history_item_id IN (
             SELECT id FROM protocol_history_items WHERE session_id = ?1
         )
           AND status IN ('pending', 'running')",
        params![session_id.to_string(), finished_at_ms],
    )?;
    Ok(())
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
    let (status, reason) = match terminal.status {
        crate::protocol::TurnTerminalStatus::Interrupted => (
            ToolCallStatus::Cancelled,
            if terminal.summary.trim().is_empty() {
                "turn interrupted before the tool call finished"
            } else {
                terminal.summary.as_str()
            },
        ),
        crate::protocol::TurnTerminalStatus::Failed => (
            ToolCallStatus::Failed,
            if terminal.summary.trim().is_empty() {
                "turn failed before the tool call finished"
            } else {
                terminal.summary.as_str()
            },
        ),
        crate::protocol::TurnTerminalStatus::Completed => (
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

    async fn active_turn(store: &StoreBundle, session_id: SessionId) -> (String, TurnId) {
        let repo = store.session_repo();
        let turn_id = TurnId::new();
        let admission_id = repo
            .admit_session_turn(session_id, turn_id)
            .await
            .expect("admit")
            .expect("admitted");
        repo.append_user_turn_with_protocol_bundle(
            session_id,
            &admission_id,
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

    async fn expire_and_recover_run(store: &StoreBundle, session_id: SessionId) -> String {
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
    }

    fn completed_terminal(session_id: SessionId, summary: &str) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                status: crate::protocol::TurnTerminalStatus::Completed,
                finish_reason: Some(FinishReason::Stop),
                interruption_cause: None,
                final_response_id: Some(ModelResponseId::new()),
                summary: summary.to_string(),
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
            .expect("first turn admitted");

        let first_state = stored_admission_state(&store, session_id);
        assert_eq!(first_state.0, "running");
        assert_eq!(first_state.1.as_deref(), Some(first_admission_id.as_str()));
        assert_eq!(first_state.2, Some(first_turn_id.to_string()));
        assert!(first_state.3.is_some());

        let terminal = completed_terminal(session_id, "first turn complete");
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    &first_admission_id,
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
            .expect("resumed turn admitted");
        let resumed_state = stored_admission_state(&store, session_id);
        assert_eq!(resumed_state.0, "running");
        assert_eq!(
            resumed_state.1.as_deref(),
            Some(resumed_admission_id.as_str())
        );
        assert_eq!(resumed_state.2, Some(resumed_turn_id.to_string()));
        assert!(resumed_state.3.is_some());
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
            (Some(admission_id), None) => (admission_id, first_turn_id),
            (None, Some(admission_id)) => (admission_id, second_turn_id),
            outcome => panic!("expected one admitted turn, got {outcome:?}"),
        };

        let state = stored_admission_state(&store, session_id);
        assert_eq!(state.0, "running");
        assert_eq!(state.1.as_deref(), Some(winning_admission_id.as_str()));
        assert_eq!(state.2, Some(winning_turn_id.to_string()));
        assert!(state.3.is_some());
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
        assert_eq!(state.1.as_deref(), Some(replacement_admission_id.as_str()));
        assert_eq!(state.2, Some(replacement_turn_id.to_string()));
        assert_eq!(
            repository
                .renew_admitted_run_lease_at(
                    session_id,
                    &expired_admission_id,
                    expired_turn_id,
                    admitted_at_ms.saturating_add(102),
                    RUN_ADMISSION_LEASE_DURATION_MS,
                )
                .await
                .expect("stale owner renewal"),
            RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
        );
        assert_eq!(
            repository
                .durable_terminal_for_turn(session_id, expired_turn_id)
                .await
                .expect("recovery terminal")
                .map(|terminal| terminal.status.as_session_status()),
            Some(SessionStatus::Failed)
        );
    }

    #[tokio::test]
    async fn fresh_legacy_null_turn_is_not_an_active_inter_agent_recipient() {
        let (store, session_id) = test_repo().await;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "UPDATE sessions
                 SET status = 'running', active_run_id = 'legacy-owner', active_turn_id = NULL,
                     active_run_lease_expires_at_ms = ?2
                 WHERE id = ?1",
                params![
                    session_id.to_string(),
                    now.saturating_add(RUN_ADMISSION_LEASE_DURATION_MS)
                ],
            )
            .expect("legacy null-turn fixture");

        let error = store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/worker".to_string(),
                    content: "do not commit to an ownerless turn".to_string(),
                    trigger_turn: true,
                },
                true,
            )
            .expect_err("null-turn recipient must not be active");
        assert!(error.to_string().contains("became terminal"));
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(session_id)
                .expect("canonical history")
                .is_empty()
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
                &admission_id,
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
                &admission_id,
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
                &admission_id,
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
    async fn rollback_is_one_transaction_and_removes_allocator_state() {
        let (store, session_id) = test_repo().await;
        let protocol = store.protocol_event_store();
        let plan_turn = TurnId::new();
        protocol
            .set_collaboration_mode(session_id, plan_turn, ModeKind::Plan)
            .expect("store plan mode")
            .expect("plan instruction");
        let default_turn = TurnId::new();
        protocol
            .set_collaboration_mode(session_id, default_turn, ModeKind::Default)
            .expect("store default mode")
            .expect("default instruction");

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
            2,
            "a reset failure must roll protocol deletion back"
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
        assert_eq!(result.dropped_turn_ids, vec![default_turn]);
        assert_eq!(result.remaining_history_items, 1);
        assert_eq!(result.session.status, SessionStatus::Idle);
        assert_eq!(
            protocol
                .collaboration_mode_for_session(session_id)
                .expect("mode after rollback"),
            ModeKind::Plan
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
                    params![session_id.to_string(), default_turn.to_string()],
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
        let root_turn = TurnId::new();
        store
            .protocol_event_store()
            .set_collaboration_mode(root_session_id, root_turn, ModeKind::Plan)
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
                &admission_id,
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
        assert_eq!(
            terminal.status,
            crate::protocol::TurnTerminalStatus::Interrupted
        );
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
                &admission_id,
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
                &admission_id,
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
                &admission_id,
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
        let terminal = completed_terminal(session_id, "done");
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    &admission_id,
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
                    &admission_id,
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
    async fn communication_for_an_inactive_recipient_starts_a_new_turn() {
        let (store, session_id) = test_repo().await;
        let (admission_id, completed_turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    &admission_id,
                    &completed_terminal(session_id, "done"),
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

        assert_ne!(communication.turn_id, completed_turn_id);
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
    async fn rollback_of_a_mail_only_future_turn_preserves_completed_turn_and_older_mail() {
        let (store, session_id) = test_repo().await;
        let (admission_id, completed_turn_id) = active_turn(&store, session_id).await;
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    &admission_id,
                    &completed_terminal(session_id, "done"),
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
        let before = store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history before rollback");
        let second_mail_turn = before
            .iter()
            .find(|item| item.id == second_mail_id)
            .expect("second mail")
            .turn_id;

        let rolled_back = store
            .session_repo()
            .rollback_session_transaction(session_id, 1)
            .await
            .expect("rollback latest mail-only turn");
        let after = store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history after rollback");

        assert_eq!(rolled_back.dropped_turn_ids, vec![second_mail_turn]);
        assert!(after.iter().any(|item| item.id == first_mail_id));
        assert!(!after.iter().any(|item| item.id == second_mail_id));
        assert!(
            store
                .session_repo()
                .durable_terminal_for_turn(session_id, completed_turn_id)
                .await
                .expect("completed terminal read")
                .is_some(),
            "rolling back future mail must not rewrite the completed turn"
        );
    }

    #[tokio::test]
    async fn admitted_terminal_is_first_writer_and_is_rehydrated_as_one_typed_value() {
        let (store, session_id) = test_repo().await;
        let (admission_id, turn_id) = active_turn(&store, session_id).await;
        let repo = store.session_repo();
        let event = completed_terminal(session_id, "done");
        assert_eq!(
            repo.terminalize_admitted_turn_with_protocol_event(
                session_id,
                &admission_id,
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
        assert_eq!(
            durable.status,
            crate::protocol::TurnTerminalStatus::Completed
        );
        assert_eq!(durable.summary, "done");
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
                &admission_id,
                &completed_terminal(session_id, "replacement"),
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
                .summary,
            "done"
        );
    }

    #[test]
    fn terminal_writer_rejects_non_terminal_events_and_contradictory_payloads() {
        let session_id = SessionId::new();
        let non_terminal = RunEvent::SessionStarted {
            session_id,
            title: "test".to_string(),
        };
        assert!(validate_terminal_event(session_id, &non_terminal).is_err());
        let contradictory = RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                status: crate::protocol::TurnTerminalStatus::Interrupted,
                finish_reason: Some(FinishReason::Stop),
                interruption_cause: None,
                final_response_id: None,
                summary: "invalid".to_string(),
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        assert!(validate_terminal_event(session_id, &contradictory).is_err());
    }
}
