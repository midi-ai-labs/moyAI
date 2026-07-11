use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::config::AccessMode;
use crate::error::StorageError;
use crate::protocol::{
    HistoryItem, HistoryItemId, HistoryItemPayload, RuntimeEvent, RuntimeEventId, RuntimeEventMsg,
    SteerTurn, ToolProgressEffect, TurnId, TurnItem, TurnItemId, TurnItemPayload, UserTurn,
    VerificationRunResult, VerificationRunStatus, insert_event_bundle_in_transaction,
    latest_turn_position_for_session, project_protocol_run_event,
};
use crate::runtime::{Clock, SystemClock};
use crate::session::{
    CompletionState, DiffSummaryPart, FailureKind, FailureState, MessageId, MessageMetadata,
    MessagePart, MessageRecord, MessageRole, NewMessage, NewPart, NewSession, PartKind, PartRecord,
    ProcessPhase, ProjectId, RunEvent, SessionId, SessionMemoryMode, SessionMemoryModeUpdate,
    SessionModelParameters, SessionRecord, SessionRepository, SessionSettingsPatch,
    SessionSettingsUpdate, SessionStateSnapshot, SessionStatus, SessionTitleUpdate, TaskRoute,
    ThreadGoal, ThreadGoalStatus, TodoItem, TodoKind, TodoPriority, TodoStatus, ToolCallId,
    ToolCallPart, ToolCallRecord, ToolCallStatus, ToolResultPart, Transcript, TranscriptMessage,
    VerificationState, validate_thread_goal_objective,
};

pub const RUN_ADMISSION_LEASE_DURATION_MS: i64 = 15_000;
pub const RUN_ADMISSION_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
const EXPIRED_RUN_RECOVERY_REASON: &str =
    "run owner lease expired before the owner acknowledged shutdown";

#[derive(Clone)]
pub struct SqliteSessionRepository {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmittedTerminalCommit {
    Applied,
    AlreadyTerminalizedBySameAdmission,
    UnseenSteer { expected: usize, actual: usize },
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

    pub async fn session_projection_state(
        &self,
        session_id: SessionId,
    ) -> Result<(bool, SessionMemoryMode), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .query_row(
                "SELECT archived_at_ms IS NOT NULL, memory_mode
                 FROM sessions WHERE id = ?1",
                params![session_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        parse_memory_mode(&row.get::<_, String>(1)?),
                    ))
                },
            )
            .map_err(StorageError::from)
    }

    pub async fn list_sessions_with_projection_state(
        &self,
        project_id: ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<(SessionRecord, bool, SessionMemoryMode)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms,
                    archived_at_ms IS NOT NULL, memory_mode
             FROM sessions
             WHERE project_id = ?1{archived_filter}
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
                        status: parse_status(&row.get::<_, String>(2)?),
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
                    parse_memory_mode(&row.get::<_, String>(12)?),
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
    ) -> Result<Vec<(SessionRecord, bool, SessionMemoryMode)>, StorageError> {
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
                    archived_at_ms IS NOT NULL, memory_mode
             FROM sessions
             WHERE project_id = ?1{archived_filter}
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
                            status: parse_status(&row.get::<_, String>(2)?),
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
                        parse_memory_mode(&row.get::<_, String>(12)?),
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub async fn insert_tool_call(
        &self,
        session_id: SessionId,
        message_id: MessageId,
        tool_name: &str,
        arguments_json: &str,
        title: Option<&str>,
        metadata_json: serde_json::Value,
    ) -> Result<ToolCallRecord, StorageError> {
        let id = ToolCallId::new();
        let started_at_ms = SystemClock::now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO tool_calls (id, session_id, message_id, tool_name, status, arguments_json, title, metadata_json, output_text, truncated_output_path, error_text, started_at_ms, finished_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7, NULL, NULL, NULL, ?8, NULL)",
            params![
                id.to_string(),
                session_id.to_string(),
                message_id.to_string(),
                tool_name,
                arguments_json,
                title,
                serde_json::to_string(&metadata_json)?,
                started_at_ms
            ],
        )?;
        Ok(ToolCallRecord {
            id,
            session_id,
            message_id,
            tool_name: parse_tool_name(tool_name),
            status: ToolCallStatus::Pending,
            arguments_json: arguments_json.to_string(),
            title: title.map(|value| value.to_string()),
            metadata_json,
            output_text: None,
            truncated_output_path: None,
            error_text: None,
            started_at_ms,
            finished_at_ms: None,
        })
    }

    pub async fn mark_tool_call_running(
        &self,
        tool_call_id: ToolCallId,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE tool_calls SET status = 'running' WHERE id = ?1",
            params![tool_call_id.to_string()],
        )?;
        Ok(())
    }

    pub async fn complete_tool_call(
        &self,
        tool_call_id: ToolCallId,
        title: &str,
        metadata_json: serde_json::Value,
        output_text: &str,
        truncated_output_path: Option<&camino::Utf8Path>,
    ) -> Result<(), StorageError> {
        let finished_at_ms = SystemClock::now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE tool_calls
             SET status = 'completed',
                 title = ?2,
                 metadata_json = ?3,
                 output_text = ?4,
                 truncated_output_path = ?5,
                 error_text = NULL,
                 finished_at_ms = ?6
             WHERE id = ?1",
            params![
                tool_call_id.to_string(),
                title,
                serde_json::to_string(&metadata_json)?,
                output_text,
                truncated_output_path.map(|value| value.as_str()),
                finished_at_ms
            ],
        )?;
        Ok(())
    }

    pub async fn fail_tool_call(
        &self,
        tool_call_id: ToolCallId,
        error_text: &str,
    ) -> Result<(), StorageError> {
        let finished_at_ms = SystemClock::now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE tool_calls
             SET status = 'failed',
                 error_text = ?2,
                 finished_at_ms = ?3
             WHERE id = ?1",
            params![tool_call_id.to_string(), error_text, finished_at_ms],
        )?;
        Ok(())
    }

    pub async fn fail_unfinished_tool_calls(
        &self,
        session_id: SessionId,
        error_text: &str,
    ) -> Result<(), StorageError> {
        let finished_at_ms = SystemClock::now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        fail_unfinished_tool_calls_in_connection(
            &connection,
            session_id,
            error_text,
            finished_at_ms,
        )?;
        Ok(())
    }

    pub async fn update_message_metadata(
        &self,
        message_id: MessageId,
        metadata: &MessageMetadata,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE messages SET metadata_json = ?2 WHERE id = ?1",
            params![message_id.to_string(), serde_json::to_string(metadata)?],
        )?;
        Ok(())
    }

    pub async fn reset_state_after_protocol_rollback(
        &self,
        session_id: SessionId,
        state: &SessionStateSnapshot,
    ) -> Result<SessionRecord, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![session_id.to_string()],
        )?;
        upsert_session_state_row(&transaction, session_id, state, now)?;
        transaction.execute(
            "UPDATE sessions
             SET status = 'idle', updated_at_ms = ?2, completed_at_ms = NULL,
                 active_run_id = NULL, active_turn_id = NULL,
                 active_run_lease_expires_at_ms = NULL
             WHERE id = ?1",
            params![session_id.to_string(), now],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_session(session_id).await
    }

    pub async fn copy_session_state_and_todos(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<(), StorageError> {
        if source_session_id == target_session_id {
            return Err(StorageError::Message(
                "cannot copy session state into the same session".to_string(),
            ));
        }
        let state = self.get_state(source_session_id).await?;
        let todos = self.list_todos(source_session_id).await?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        upsert_session_state_row(&transaction, target_session_id, &state, now)?;
        transaction.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![target_session_id.to_string()],
        )?;
        for (position, todo) in todos.iter().enumerate() {
            transaction.execute(
                "INSERT INTO session_todos (
                     session_id, todo_id, position, content, kind, status, priority, targets_json,
                     depends_on_json, success_criteria_json, blocked_by_json
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    target_session_id.to_string(),
                    todo.id.to_string(),
                    position as i64,
                    todo.content,
                    todo_kind_text(todo.kind),
                    todo_status_text(todo.status),
                    todo_priority_text(todo.priority),
                    serde_json::to_string(&todo.targets)?,
                    serde_json::to_string(&todo.depends_on)?,
                    serde_json::to_string(&todo.success_criteria)?,
                    serde_json::to_string(&todo.blocked_by)?
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub async fn get_session_memory_mode(
        &self,
        id: SessionId,
    ) -> Result<SessionMemoryMode, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let value: String = connection.query_row(
            "SELECT memory_mode FROM sessions WHERE id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )?;
        Ok(parse_memory_mode(&value))
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

    pub async fn append_user_message_with_protocol_bundle(
        &self,
        draft: NewMessage,
        admission_id: &str,
        parts: Vec<NewPart>,
        initial_state: &SessionStateSnapshot,
        turn: &UserTurn,
        protocol_turn_id: TurnId,
        protocol_sequence_no: i64,
    ) -> Result<MessageRecord, StorageError> {
        let id = MessageId::new();
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let metadata_json = serde_json::to_string(&draft.metadata)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let activated = transaction.execute(
            "UPDATE sessions
             SET status = 'running',
                 updated_at_ms = ?4,
                 completed_at_ms = NULL,
                 active_turn_id = ?3
             WHERE id = ?1
               AND active_run_id = ?2
               AND status = 'running'
               AND active_turn_id IS NULL
               AND active_run_lease_expires_at_ms > ?5",
            params![
                draft.session_id.to_string(),
                admission_id,
                protocol_turn_id.to_string(),
                now,
                now
            ],
        )? == 1;
        if !activated {
            return Err(StorageError::Message(format!(
                "run admission {admission_id} no longer owns session {} while storing its user turn",
                draft.session_id
            )));
        }
        let sequence_no = next_message_sequence_in_transaction(&transaction, draft.session_id)?;

        transaction.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![draft.session_id.to_string()],
        )?;
        upsert_session_state_row(&transaction, draft.session_id, initial_state, now)?;
        transaction.execute(
            "INSERT INTO messages (id, session_id, parent_message_id, role, sequence_no, metadata_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_string(),
                draft.session_id.to_string(),
                draft.parent_message_id.map(|value| value.to_string()),
                match draft.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                },
                sequence_no,
                metadata_json,
                now
            ],
        )?;
        for (part_sequence_no, part) in parts.into_iter().enumerate() {
            transaction.execute(
                "INSERT INTO message_parts (id, message_id, sequence_no, part_kind, payload_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    crate::session::PartId::new().to_string(),
                    id.to_string(),
                    part_sequence_no as i64,
                    part_kind_text(part.kind),
                    serde_json::to_string(&part.payload)?,
                    now
                ],
            )?;
        }
        let run_event = crate::session::RunEvent::UserTurnStored {
            session_id: draft.session_id,
            message_id: id,
            turn: Box::new(turn.clone()),
        };
        let projection = project_protocol_run_event(
            &run_event,
            Some(draft.session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )
        .ok_or_else(|| {
            StorageError::Message("UserTurnStored did not produce protocol projection".to_string())
        })?;
        crate::protocol::insert_event_bundle_in_transaction(
            &transaction,
            &projection.runtime_event,
            projection.history_item.as_ref(),
            projection.turn_item.as_ref(),
        )?;
        transaction.commit()?;

        Ok(MessageRecord {
            id,
            session_id: draft.session_id,
            role: draft.role,
            parent_message_id: draft.parent_message_id,
            sequence_no,
            created_at_ms: now,
            metadata: draft.metadata,
        })
    }

    pub async fn append_assistant_message_with_protocol_start(
        &self,
        draft: NewMessage,
        admission_id: &str,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        model: String,
    ) -> Result<(MessageRecord, RunEvent), StorageError> {
        let id = MessageId::new();
        let now = SystemClock.now_ms();
        let metadata_json = serde_json::to_string(&draft.metadata)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            draft.session_id,
            admission_id,
            protocol_turn_id,
        )?;
        let sequence_no = next_message_sequence_in_transaction(&transaction, draft.session_id)?;
        transaction.execute(
            "INSERT INTO messages (id, session_id, parent_message_id, role, sequence_no, metadata_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_string(),
                draft.session_id.to_string(),
                draft.parent_message_id.map(|value| value.to_string()),
                match draft.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                },
                sequence_no,
                metadata_json,
                now
            ],
        )?;
        let event = RunEvent::AssistantStarted {
            message_id: id,
            model,
        };
        insert_protocol_projection_if_requested(
            &transaction,
            &event,
            Some(draft.session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        transaction.commit()?;
        Ok((
            MessageRecord {
                id,
                session_id: draft.session_id,
                role: draft.role,
                parent_message_id: draft.parent_message_id,
                sequence_no,
                created_at_ms: now,
                metadata: draft.metadata,
            },
            event,
        ))
    }

    pub async fn append_message_with_parts_and_protocol_event(
        &self,
        draft: NewMessage,
        parts: Vec<NewPart>,
        event_factory: impl FnOnce(MessageId) -> RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<(MessageRecord, RunEvent), StorageError> {
        let id = MessageId::new();
        let now = SystemClock.now_ms();
        let metadata_json = serde_json::to_string(&draft.metadata)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let sequence_no = next_message_sequence_in_transaction(&transaction, draft.session_id)?;
        transaction.execute(
            "INSERT INTO messages (id, session_id, parent_message_id, role, sequence_no, metadata_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_string(),
                draft.session_id.to_string(),
                draft.parent_message_id.map(|value| value.to_string()),
                match draft.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                },
                sequence_no,
                metadata_json,
                now
            ],
        )?;
        for part in parts {
            insert_part_in_transaction(&transaction, id, part)?;
        }
        let event = event_factory(id);
        insert_protocol_projection_if_requested(
            &transaction,
            &event,
            Some(draft.session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        transaction.commit()?;
        Ok((
            MessageRecord {
                id,
                session_id: draft.session_id,
                role: draft.role,
                parent_message_id: draft.parent_message_id,
                sequence_no,
                created_at_ms: now,
                metadata: draft.metadata,
            },
            event,
        ))
    }

    pub async fn append_part_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: &str,
        message_id: MessageId,
        part: NewPart,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<PartRecord, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        let record = insert_part_in_transaction(&transaction, message_id, part)?;
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        transaction.commit()?;
        Ok(record)
    }

    pub async fn update_state_with_protocol_event(
        &self,
        session_id: SessionId,
        state: &SessionStateSnapshot,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<(), StorageError> {
        let now = SystemClock.now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        upsert_session_state_row(&transaction, session_id, state, now)?;
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub async fn update_session_title_with_protocol_event(
        &self,
        session_id: SessionId,
        title: &str,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<(), StorageError> {
        let now = SystemClock.now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE sessions SET title = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![session_id.to_string(), title, now],
        )?;
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub async fn set_status_with_protocol_event(
        &self,
        session_id: SessionId,
        status: SessionStatus,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<(), StorageError> {
        let now = SystemClock.now_ms();
        let status_text = match status {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Completed => "completed",
            SessionStatus::AwaitingUser => "awaiting_user",
            SessionStatus::Cancelled => "cancelled",
            SessionStatus::Failed => "failed",
        };
        let completed_at_ms = if matches!(
            status,
            SessionStatus::Completed
                | SessionStatus::AwaitingUser
                | SessionStatus::Cancelled
                | SessionStatus::Failed
        ) {
            Some(now)
        } else {
            None
        };
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE sessions
             SET status = ?2,
                 updated_at_ms = ?3,
                 completed_at_ms = ?4
             WHERE id = ?1",
            params![session_id.to_string(), status_text, now, completed_at_ms],
        )?;
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub async fn admit_session_run(
        &self,
        session_id: SessionId,
    ) -> Result<Option<String>, StorageError> {
        self.admit_session_run_at(
            session_id,
            SystemClock::now_ms(),
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
    }

    pub async fn admit_session_run_at(
        &self,
        session_id: SessionId,
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
        } else if matches!(status.as_str(), "running" | "awaiting_user") {
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
                 active_turn_id = NULL,
                 active_run_lease_expires_at_ms = ?4
             WHERE id = ?1
               AND active_run_id IS NULL
               AND status IN ('idle', 'completed', 'cancelled', 'failed')",
            params![
                session_id.to_string(),
                now,
                admission_id,
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
                    && active_turn_id
                        .as_deref()
                        .is_none_or(|active_turn_id| active_turn_id == turn_id.to_string())
                    && run_lease_is_fresh(lease_expires_at_ms, now)
                    && matches!(status.as_str(), "running" | "awaiting_user") =>
            {
                let renewed = transaction.execute(
                    "UPDATE sessions
                     SET active_run_lease_expires_at_ms = MAX(
                             active_run_lease_expires_at_ms,
                             ?4
                         )
                     WHERE id = ?1
                       AND active_run_id = ?2
                       AND (active_turn_id IS NULL OR active_turn_id = ?3)
                       AND active_run_lease_expires_at_ms > ?5
                       AND status IN ('running', 'awaiting_user')",
                    params![
                        session_id.to_string(),
                        admission_id,
                        turn_id.to_string(),
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
                        && active_turn_id.as_deref().is_none_or(|active_turn_id| {
                            active_turn_id == turn_id.to_string()
                        })
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

    pub async fn try_admit_session_run(&self, session_id: SessionId) -> Result<bool, StorageError> {
        Ok(self.admit_session_run(session_id).await?.is_some())
    }

    pub async fn activate_admitted_turn(
        &self,
        session_id: SessionId,
        admission_id: &str,
        turn_id: TurnId,
    ) -> Result<bool, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let activated = connection.execute(
            "UPDATE sessions
             SET active_turn_id = ?3, updated_at_ms = ?4
             WHERE id = ?1
               AND active_run_id = ?2
               AND status = 'running'
               AND (active_turn_id IS NULL OR active_turn_id = ?3)
               AND active_run_lease_expires_at_ms > ?5",
            params![
                session_id.to_string(),
                admission_id,
                turn_id.to_string(),
                now,
                now
            ],
        )? == 1;
        Ok(activated)
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
            .map(|status| parse_status(&status));
        Ok(status)
    }

    pub async fn corroborated_terminal_status_for_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<SessionStatus>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let session_status = transaction
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|status| parse_status(&status));
        let Some(session_status) = session_status else {
            transaction.commit()?;
            return Ok(None);
        };
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
        let mut protocol_terminal_status = None;
        for row in rows {
            let msg = serde_json::from_str::<RuntimeEventMsg>(&row?)?;
            protocol_terminal_status = match msg {
                RuntimeEventMsg::TurnCompleted { .. } => Some(SessionStatus::Completed),
                RuntimeEventMsg::TurnAwaitingUser { .. } => Some(SessionStatus::AwaitingUser),
                RuntimeEventMsg::TurnInterrupted { .. } => Some(SessionStatus::Cancelled),
                RuntimeEventMsg::TurnFailed { .. } => Some(SessionStatus::Failed),
                _ => None,
            };
            if protocol_terminal_status.is_some() {
                break;
            }
        }
        drop(statement);
        transaction.commit()?;
        Ok(protocol_terminal_status.filter(|status| *status == session_status))
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
                   AND status IN ('running', 'awaiting_user')",
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
                   AND status IN ('running', 'awaiting_user')",
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
               AND status NOT IN ('running', 'awaiting_user')",
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
        if !matches!(status.as_str(), "running" | "awaiting_user") {
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
        let stored = insert_event_bundle_in_transaction(
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
                   AND status IN ('running', 'awaiting_user')
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
        status: SessionStatus,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<bool, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let status_text = match status {
            SessionStatus::Completed => "completed",
            SessionStatus::AwaitingUser => "awaiting_user",
            SessionStatus::Cancelled => "cancelled",
            SessionStatus::Failed => "failed",
            SessionStatus::Idle | SessionStatus::Running => {
                return Err(StorageError::Message(
                    "active session terminalization requires a terminal status".to_string(),
                ));
            }
        };
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let protocol_sequence_no = match protocol_sequence_no {
            Some(sequence_no) => sequence_no,
            None => latest_turn_position_for_session(&transaction, session_id)?
                .filter(|(turn_id, _)| *turn_id == protocol_turn_id)
                .map(|(_, sequence_no)| sequence_no)
                .unwrap_or(0),
        };
        let terminalized = transaction.execute(
            "UPDATE sessions
             SET status = ?2, updated_at_ms = ?3, completed_at_ms = ?3
             WHERE id = ?1
               AND status IN ('running', 'awaiting_user')
               AND (active_turn_id IS NULL OR active_turn_id = ?4)",
            params![
                session_id.to_string(),
                status_text,
                now,
                protocol_turn_id.to_string()
            ],
        )? == 1;
        if terminalized {
            if status != SessionStatus::AwaitingUser {
                fail_unfinished_tool_calls_in_connection(
                    &transaction,
                    session_id,
                    unfinished_tool_reason_for_event(event),
                    now,
                )?;
            }
            insert_protocol_projection_if_requested(
                &transaction,
                event,
                Some(session_id),
                protocol_turn_id,
                Some(protocol_sequence_no),
            )?;
        }
        transaction.commit()?;
        Ok(terminalized)
    }

    pub async fn terminalize_admitted_session_with_protocol_event(
        &self,
        session_id: SessionId,
        admission_id: &str,
        status: SessionStatus,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<bool, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let status_text = terminal_status_text(status)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_state = transaction
            .query_row(
                "SELECT status, active_turn_id, active_run_lease_expires_at_ms
                 FROM sessions
                 WHERE id = ?1 AND active_run_id = ?2",
                params![session_id.to_string(), admission_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((current_status, active_turn_id, lease_expires_at_ms)) = current_state else {
            transaction.commit()?;
            return Ok(false);
        };
        if !run_lease_is_fresh(lease_expires_at_ms, now) {
            transaction.commit()?;
            return Ok(false);
        }
        if active_turn_id
            .as_deref()
            .is_some_and(|active_turn_id| active_turn_id != protocol_turn_id.to_string())
        {
            transaction.commit()?;
            return Ok(false);
        }
        let was_active = matches!(current_status.as_str(), "running" | "awaiting_user");
        if !was_active {
            transaction.execute(
                "UPDATE sessions
                 SET active_run_id = NULL,
                     active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1 AND active_run_id = ?2",
                params![session_id.to_string(), admission_id],
            )?;
            fail_unfinished_tool_calls_in_connection(
                &transaction,
                session_id,
                unfinished_tool_reason_for_event(event),
                now,
            )?;
            transaction.commit()?;
            return Ok(true);
        }
        if status == SessionStatus::AwaitingUser {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?3, updated_at_ms = ?4, completed_at_ms = ?4
                 WHERE id = ?1 AND active_run_id = ?2",
                params![session_id.to_string(), admission_id, status_text, now],
            )?;
        } else {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?3,
                     updated_at_ms = ?4,
                     completed_at_ms = ?4,
                     active_run_id = NULL,
                     active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1 AND active_run_id = ?2",
                params![session_id.to_string(), admission_id, status_text, now],
            )?;
            fail_unfinished_tool_calls_in_connection(
                &transaction,
                session_id,
                unfinished_tool_reason_for_event(event),
                now,
            )?;
        }
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            Some(protocol_sequence_no.unwrap_or(0)),
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub async fn record_pending_tool_call_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: &str,
        message_id: MessageId,
        tool_name: &str,
        arguments_json: &str,
        title: Option<&str>,
        metadata_json: serde_json::Value,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<(ToolCallRecord, RunEvent), StorageError> {
        let id = ToolCallId::new();
        let started_at_ms = SystemClock::now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        transaction.execute(
            "INSERT INTO tool_calls (id, session_id, message_id, tool_name, status, arguments_json, title, metadata_json, output_text, truncated_output_path, error_text, started_at_ms, finished_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7, NULL, NULL, NULL, ?8, NULL)",
            params![
                id.to_string(),
                session_id.to_string(),
                message_id.to_string(),
                tool_name,
                arguments_json,
                title,
                serde_json::to_string(&metadata_json)?,
                started_at_ms
            ],
        )?;
        let parsed_tool_name = parse_tool_name(tool_name);
        let event = RunEvent::ToolCallPending {
            tool_call_id: id,
            tool: parsed_tool_name,
            title: title.unwrap_or(tool_name).to_string(),
            metadata: metadata_json.clone(),
        };
        insert_protocol_projection_if_requested(
            &transaction,
            &event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        insert_part_in_transaction(
            &transaction,
            message_id,
            NewPart {
                kind: PartKind::ToolCall,
                payload: MessagePart::ToolCall(ToolCallPart {
                    tool_call_id: id,
                    tool_name: parsed_tool_name,
                    arguments_json: arguments_json.to_string(),
                    model_arguments_json: metadata_json
                        .get("tool_route")
                        .and_then(|route| route.get("original_arguments_json"))
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string),
                    effective_arguments_json: Some(arguments_json.to_string()),
                }),
            },
        )?;
        transaction.commit()?;
        Ok((
            ToolCallRecord {
                id,
                session_id,
                message_id,
                tool_name: parsed_tool_name,
                status: ToolCallStatus::Pending,
                arguments_json: arguments_json.to_string(),
                title: title.map(|value| value.to_string()),
                metadata_json,
                output_text: None,
                truncated_output_path: None,
                error_text: None,
                started_at_ms,
                finished_at_ms: None,
            },
            event,
        ))
    }

    pub async fn complete_tool_call_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: &str,
        message_id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        title: &str,
        metadata_json: serde_json::Value,
        output_text: &str,
        truncated_output_path: Option<&camino::Utf8Path>,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<RunEvent, StorageError> {
        let finished_at_ms = SystemClock::now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        transaction.execute(
            "UPDATE tool_calls
             SET status = 'completed',
                 title = ?2,
                 metadata_json = ?3,
                 output_text = ?4,
                 truncated_output_path = ?5,
                 error_text = NULL,
                 finished_at_ms = ?6
             WHERE id = ?1",
            params![
                tool_call_id.to_string(),
                title,
                serde_json::to_string(&metadata_json)?,
                output_text,
                truncated_output_path.map(|value| value.as_str()),
                finished_at_ms
            ],
        )?;
        let event = RunEvent::ToolCallCompleted {
            tool_call_id,
            tool: tool_name,
            title: title.to_string(),
            summary: output_text.to_string(),
            metadata: metadata_json.clone(),
        };
        insert_protocol_projection_if_requested(
            &transaction,
            &event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        insert_part_in_transaction(
            &transaction,
            message_id,
            NewPart {
                kind: PartKind::ToolResult,
                payload: MessagePart::ToolResult(ToolResultPart {
                    tool_call_id,
                    status: ToolCallStatus::Completed,
                    title: title.to_string(),
                    summary: output_text.to_string(),
                    success: tool_success_from_metadata(&metadata_json),
                    progress_effect: tool_progress_effect_from_metadata(&metadata_json),
                    blocked_action: metadata_string(&metadata_json, &["blocked_action"]),
                    result_hash: metadata_string(
                        &metadata_json,
                        &["tool_feedback_envelope", "result_hash"],
                    )
                    .or_else(|| metadata_string(&metadata_json, &["result_hash"])),
                }),
            },
        )?;
        transaction.commit()?;
        Ok(event)
    }

    pub async fn complete_tool_call_with_file_changes_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: &str,
        message_id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        title: &str,
        metadata_json: serde_json::Value,
        output_text: &str,
        truncated_output_path: Option<&camino::Utf8Path>,
        diff_summary: DiffSummaryPart,
        file_changes: Vec<crate::edit::ChangeSummary>,
        protocol_turn_id: TurnId,
        tool_output_sequence_no: Option<i64>,
        file_changes_sequence_no: Option<i64>,
    ) -> Result<(RunEvent, RunEvent), StorageError> {
        validate_file_change_protocol_bundle(tool_call_id, &diff_summary, &file_changes)?;
        let finished_at_ms = SystemClock::now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        transaction.execute(
            "UPDATE tool_calls
             SET status = 'completed',
                 title = ?2,
                 metadata_json = ?3,
                 output_text = ?4,
                 truncated_output_path = ?5,
                 error_text = NULL,
                 finished_at_ms = ?6
             WHERE id = ?1",
            params![
                tool_call_id.to_string(),
                title,
                serde_json::to_string(&metadata_json)?,
                output_text,
                truncated_output_path.map(|value| value.as_str()),
                finished_at_ms
            ],
        )?;
        let tool_output_event = RunEvent::ToolCallCompleted {
            tool_call_id,
            tool: tool_name,
            title: title.to_string(),
            summary: output_text.to_string(),
            metadata: metadata_json.clone(),
        };
        let file_changes_event = RunEvent::FileChangesRecorded {
            tool_call_id,
            changes: file_changes,
        };
        insert_protocol_projection_if_requested(
            &transaction,
            &tool_output_event,
            Some(session_id),
            protocol_turn_id,
            tool_output_sequence_no,
        )?;
        insert_part_in_transaction(
            &transaction,
            message_id,
            NewPart {
                kind: PartKind::ToolResult,
                payload: MessagePart::ToolResult(ToolResultPart {
                    tool_call_id,
                    status: ToolCallStatus::Completed,
                    title: title.to_string(),
                    summary: output_text.to_string(),
                    success: tool_success_from_metadata(&metadata_json),
                    progress_effect: tool_progress_effect_from_metadata(&metadata_json),
                    blocked_action: metadata_string(&metadata_json, &["blocked_action"]),
                    result_hash: metadata_string(
                        &metadata_json,
                        &["tool_feedback_envelope", "result_hash"],
                    )
                    .or_else(|| metadata_string(&metadata_json, &["result_hash"])),
                }),
            },
        )?;
        insert_part_in_transaction(
            &transaction,
            message_id,
            NewPart {
                kind: PartKind::DiffSummary,
                payload: MessagePart::DiffSummary(diff_summary),
            },
        )?;
        insert_protocol_projection_if_requested(
            &transaction,
            &file_changes_event,
            Some(session_id),
            protocol_turn_id,
            file_changes_sequence_no,
        )?;
        transaction.commit()?;
        Ok((tool_output_event, file_changes_event))
    }

    pub async fn fail_tool_call_with_protocol_bundle(
        &self,
        session_id: SessionId,
        admission_id: &str,
        message_id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        error_text: &str,
        metadata_json: serde_json::Value,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<RunEvent, StorageError> {
        let finished_at_ms = SystemClock::now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            session_id,
            admission_id,
            protocol_turn_id,
        )?;
        transaction.execute(
            "UPDATE tool_calls
             SET status = 'failed',
                 error_text = ?2,
                 finished_at_ms = ?3
             WHERE id = ?1",
            params![tool_call_id.to_string(), error_text, finished_at_ms],
        )?;
        let event = RunEvent::ToolCallFailed {
            tool_call_id,
            tool: tool_name,
            error: error_text.to_string(),
            metadata: metadata_json.clone(),
        };
        insert_protocol_projection_if_requested(
            &transaction,
            &event,
            Some(session_id),
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        insert_part_in_transaction(
            &transaction,
            message_id,
            NewPart {
                kind: PartKind::ToolResult,
                payload: MessagePart::ToolResult(ToolResultPart {
                    tool_call_id,
                    status: ToolCallStatus::Failed,
                    title: "Tool failed".to_string(),
                    summary: error_text.to_string(),
                    success: Some(false),
                    progress_effect: ToolProgressEffect::Blocked,
                    blocked_action: metadata_string(&metadata_json, &["blocked_action"]),
                    result_hash: metadata_string(
                        &metadata_json,
                        &["tool_feedback_envelope", "result_hash"],
                    )
                    .or_else(|| metadata_string(&metadata_json, &["result_hash"])),
                }),
            },
        )?;
        transaction.commit()?;
        Ok(event)
    }

    pub async fn update_message_metadata_and_status_with_protocol_event(
        &self,
        session_id: SessionId,
        message_id: MessageId,
        metadata: &MessageMetadata,
        status: SessionStatus,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<bool, StorageError> {
        Ok(self
            .update_message_metadata_and_status_with_protocol_event_guarded(
                session_id,
                message_id,
                metadata,
                status,
                event,
                protocol_turn_id,
                protocol_sequence_no,
                None,
                None,
                None,
            )
            .await?
            .was_applied())
    }

    pub async fn update_admitted_message_metadata_and_status_with_protocol_event(
        &self,
        session_id: SessionId,
        admission_id: &str,
        message_id: MessageId,
        metadata: &MessageMetadata,
        status: SessionStatus,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        expected_seen_steer_count: Option<usize>,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        self.update_message_metadata_and_status_with_protocol_event_guarded(
            session_id,
            message_id,
            metadata,
            status,
            event,
            protocol_turn_id,
            protocol_sequence_no,
            Some(admission_id),
            expected_seen_steer_count,
            expected_active_goal_id_to_block,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_message_metadata_and_status_with_protocol_event_guarded(
        &self,
        session_id: SessionId,
        message_id: MessageId,
        metadata: &MessageMetadata,
        status: SessionStatus,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        admission_id: Option<&str>,
        expected_seen_steer_count: Option<usize>,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        let now = SystemClock.now_ms();
        let status_text = session_status_text(status);
        let completed_at_ms = if matches!(
            status,
            SessionStatus::Completed
                | SessionStatus::AwaitingUser
                | SessionStatus::Cancelled
                | SessionStatus::Failed
        ) {
            Some(now)
        } else {
            None
        };
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_status = if let Some(admission_id) = admission_id {
            let owned_state = transaction
                .query_row(
                    "SELECT status, active_run_lease_expires_at_ms
                     FROM sessions
                     WHERE id = ?1 AND active_run_id = ?2 AND active_turn_id = ?3",
                    params![
                        session_id.to_string(),
                        admission_id,
                        protocol_turn_id.to_string()
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
                )
                .optional()?;
            let Some((owned_status, lease_expires_at_ms)) = owned_state else {
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::NotOwned);
            };
            if !run_lease_is_fresh(lease_expires_at_ms, now) {
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::NotOwned);
            }
            if !matches!(owned_status.as_str(), "running" | "awaiting_user") {
                transaction.execute(
                    "UPDATE messages SET metadata_json = ?2 WHERE id = ?1",
                    params![message_id.to_string(), serde_json::to_string(metadata)?],
                )?;
                transaction.execute(
                    "UPDATE sessions
                     SET active_run_id = NULL,
                         active_turn_id = NULL,
                         active_run_lease_expires_at_ms = NULL
                     WHERE id = ?1 AND active_run_id = ?2 AND active_turn_id = ?3",
                    params![
                        session_id.to_string(),
                        admission_id,
                        protocol_turn_id.to_string()
                    ],
                )?;
                fail_unfinished_tool_calls_in_connection(
                    &transaction,
                    session_id,
                    unfinished_tool_reason_for_event(event),
                    now,
                )?;
                transaction.commit()?;
                return Ok(AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission);
            }
            Some(owned_status)
        } else {
            transaction
                .query_row(
                    "SELECT status FROM sessions WHERE id = ?1",
                    params![session_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        };
        if !current_status
            .as_deref()
            .is_some_and(|value| matches!(value, "running" | "awaiting_user"))
        {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::NotOwned);
        }
        let steer_count_mismatch = match expected_seen_steer_count {
            Some(expected) => {
                let actual = count_steer_history_items(&transaction, session_id)?;
                (actual != expected).then_some((expected, actual))
            }
            None => None,
        };
        if let Some((expected, actual)) = steer_count_mismatch {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::UnseenSteer { expected, actual });
        }
        let terminalized = if let Some(admission_id) = admission_id {
            if status == SessionStatus::AwaitingUser {
                transaction.execute(
                    "UPDATE sessions
                     SET status = ?4, updated_at_ms = ?5, completed_at_ms = ?6
                     WHERE id = ?1
                       AND active_run_id = ?2
                       AND active_turn_id = ?3
                       AND active_run_lease_expires_at_ms > ?7
                       AND status IN ('running', 'awaiting_user')",
                    params![
                        session_id.to_string(),
                        admission_id,
                        protocol_turn_id.to_string(),
                        status_text,
                        now,
                        completed_at_ms,
                        now
                    ],
                )? == 1
            } else {
                transaction.execute(
                    "UPDATE sessions
                     SET status = ?4,
                         updated_at_ms = ?5,
                         completed_at_ms = ?6,
                         active_run_id = NULL,
                         active_turn_id = NULL,
                         active_run_lease_expires_at_ms = NULL
                     WHERE id = ?1
                       AND active_run_id = ?2
                       AND active_turn_id = ?3
                       AND active_run_lease_expires_at_ms > ?7
                       AND status IN ('running', 'awaiting_user')",
                    params![
                        session_id.to_string(),
                        admission_id,
                        protocol_turn_id.to_string(),
                        status_text,
                        now,
                        completed_at_ms,
                        now
                    ],
                )? == 1
            }
        } else {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?2,
                     updated_at_ms = ?3,
                     completed_at_ms = ?4,
                     active_run_id = NULL,
                     active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1 AND status IN ('running', 'awaiting_user')",
                params![session_id.to_string(), status_text, now, completed_at_ms],
            )? == 1
        };
        if !terminalized {
            transaction.commit()?;
            return Ok(AdmittedTerminalCommit::NotOwned);
        }
        transaction.execute(
            "UPDATE messages SET metadata_json = ?2 WHERE id = ?1",
            params![message_id.to_string(), serde_json::to_string(metadata)?],
        )?;
        if status == SessionStatus::Failed
            && let Some(expected_goal_id) = expected_active_goal_id_to_block
        {
            transaction.execute(
                "UPDATE thread_goals
                 SET status = 'blocked',
                     updated_at_ms = MAX(updated_at_ms + 1, ?3)
                 WHERE thread_id = ?1
                   AND goal_id = ?2
                   AND status = 'active'",
                params![session_id.to_string(), expected_goal_id, now],
            )?;
        }
        if status != SessionStatus::AwaitingUser {
            fail_unfinished_tool_calls_in_connection(
                &transaction,
                session_id,
                unfinished_tool_reason_for_event(event),
                now,
            )?;
        }
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            Some(protocol_sequence_no.unwrap_or(0)),
        )?;
        transaction.commit()?;
        Ok(AdmittedTerminalCommit::Applied)
    }

    pub async fn compatibility_transcript(
        &self,
        session_id: SessionId,
    ) -> Result<Transcript, StorageError> {
        let session = self.get_session(session_id).await?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut stmt = connection.prepare(
            "SELECT id, role, parent_message_id, sequence_no, metadata_json, created_at_ms
             FROM messages WHERE session_id = ?1 ORDER BY sequence_no ASC",
        )?;
        let message_rows = stmt
            .query_map(params![session_id.to_string()], |row| {
                let id_text: String = row.get(0)?;
                let role_text: String = row.get(1)?;
                let metadata_json: String = row.get(4)?;
                Ok(MessageRecord {
                    id: id_text.parse().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    session_id,
                    role: if role_text == "user" {
                        MessageRole::User
                    } else {
                        MessageRole::Assistant
                    },
                    parent_message_id: row
                        .get::<_, Option<String>>(2)?
                        .map(|value| value.parse())
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    sequence_no: row.get(3)?,
                    metadata: serde_json::from_str(&metadata_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    created_at_ms: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut messages = Vec::new();
        for record in message_rows {
            let mut part_stmt = connection.prepare(
                "SELECT id, sequence_no, part_kind, payload_json
                 FROM message_parts WHERE message_id = ?1 ORDER BY sequence_no ASC",
            )?;
            let parts = part_stmt
                .query_map(params![record.id.to_string()], |row| {
                    let id_text: String = row.get(0)?;
                    let kind_text: String = row.get(2)?;
                    let payload_json: String = row.get(3)?;
                    let payload = serde_json::from_str(&payload_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(PartRecord {
                        id: id_text.parse().map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        message_id: record.id,
                        sequence_no: row.get(1)?,
                        kind: parse_part_kind(&kind_text),
                        payload,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            messages.push(TranscriptMessage { record, parts });
        }

        Ok(Transcript { session, messages })
    }
}

#[async_trait(?Send)]
impl SessionRepository for SqliteSessionRepository {
    async fn create_session(&self, draft: NewSession) -> Result<SessionRecord, StorageError> {
        let id = SessionId::new();
        let now = SystemClock.now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO sessions (id, project_id, title, status, cwd_path, model_name, base_url, access_mode, memory_mode, model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'enabled', '{}', ?9, ?10, NULL)",
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
        upsert_session_state_row(&connection, id, &SessionStateSnapshot::default(), now)?;
        drop(connection);
        self.get_session(id).await
    }

    async fn get_session(&self, id: SessionId) -> Result<SessionRecord, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.query_row(
            "SELECT project_id, title, status, cwd_path, model_name, base_url, access_mode, model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms
             FROM sessions WHERE id = ?1",
            params![id.to_string()],
            |row| {
                Ok(SessionRecord {
                    id,
                    project_id: row
                        .get::<_, String>(0)?
                        .parse()
                        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))?,
                    title: row.get(1)?,
                    status: parse_status(&row.get::<_, String>(2)?),
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
        ).map_err(StorageError::from)
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
                 WHERE id = ?1 AND status NOT IN ('running', 'awaiting_user')",
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
            if matches!(
                current.status,
                SessionStatus::Running | SessionStatus::AwaitingUser
            ) {
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
            if matches!(
                current.status,
                SessionStatus::Running | SessionStatus::AwaitingUser
            ) {
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
                   AND status NOT IN ('running', 'awaiting_user')",
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

    async fn update_session_memory_mode(
        &self,
        id: SessionId,
        mode: SessionMemoryMode,
    ) -> Result<SessionMemoryModeUpdate, StorageError> {
        let current = self.get_session(id).await?;
        let current_mode = self.get_session_memory_mode(id).await?;
        if current_mode == mode {
            return Ok(SessionMemoryModeUpdate {
                session: current,
                mode,
                changed: false,
            });
        }
        let now = SystemClock::now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE sessions SET memory_mode = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), mode.key(), now],
        )?;
        drop(connection);
        Ok(SessionMemoryModeUpdate {
            session: self.get_session(id).await?,
            mode,
            changed: true,
        })
    }

    async fn delete_session(&self, id: SessionId) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let tx = connection.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM harness_replay_reports
             WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_gate_results
             WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_contracts
             WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_artifacts
             WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_events
             WHERE run_id IN (SELECT id FROM harness_runs WHERE session_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_runs WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_turn_items WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_history_items WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_runtime_events WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_item_append_order WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_turn_sequence_allocators WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM file_changes WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM tool_calls WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM message_parts
             WHERE message_id IN (SELECT id FROM messages WHERE session_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM session_state WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    async fn get_state(&self, session_id: SessionId) -> Result<SessionStateSnapshot, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let row = connection
            .query_row(
                "SELECT task_route, phase, review_scope_json, active_todo_id, active_targets_json, contract_refs_json, failure_kind, failure_summary, failure_tool_name, failure_targets_json,
                        verification_todo_id, verification_commands_json, verification_failures_json, verification_evidence_summary,
                        completion_closeout_ready, completion_open_work_count, completion_verification_pending, completion_route_contract_pending,
                        completion_blocked_reason, completion_route_contract_summary, docs_route_state_json, implementation_handoff_json,
                        verification_failure_cluster_json, verification_requirement_refs_json, token_accounting_json
                 FROM session_state
                 WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| {
                    let active_todo_id = row
                        .get::<_, Option<String>>(3)?
                        .map(|value| value.parse())
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    let failure_kind = row
                        .get::<_, Option<String>>(6)?
                        .map(|value| parse_failure_kind(&value))
                        .transpose()?;
                    let failure_tool_name = row
                        .get::<_, Option<String>>(8)?
                        .map(|value| parse_tool_name(&value));
                    let verification_todo_id = row
                        .get::<_, Option<String>>(10)?
                        .map(|value| value.parse())
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                10,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    let review_scope_json: String = row.get(2)?;
                    let active_targets_json: String = row.get(4)?;
                    let contract_refs_json: String = row.get(5)?;
                    let failure_targets_json: String = row.get(9)?;
                    let verification_commands_json: String = row.get(11)?;
                    let verification_failures_json: String = row.get(12)?;
                    let docs_route_state_json: String = row.get(20)?;
                    let implementation_handoff_json: String = row.get(21)?;
                    let verification_failure_cluster_json: String = row.get(22)?;
                    let verification_requirement_refs_json: String = row.get(23)?;
                    let token_accounting_json: String = row.get(24)?;
                    let failure = match failure_kind {
                        Some(kind) => Some(FailureState {
                            kind,
                            summary: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                            tool_name: failure_tool_name,
                            targets: serde_json::from_str(&failure_targets_json).map_err(
                                |error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        9,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                },
                            )?,
                        }),
                        None => None,
                    };
                    Ok(SessionStateSnapshot {
                        route: parse_task_route(&row.get::<_, String>(0)?)?,
                        process_phase: parse_process_phase(&row.get::<_, String>(1)?)?,
                        review_scope: serde_json::from_str(&review_scope_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        active_todo_id,
                        active_targets: serde_json::from_str(&active_targets_json).map_err(
                            |error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    4,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            },
                        )?,
                        contract_refs: serde_json::from_str(&contract_refs_json).map_err(
                            |error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    5,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            },
                        )?,
                        failure,
                        verification: VerificationState {
                            pending_todo_id: verification_todo_id,
                            required_commands: serde_json::from_str(&verification_commands_json)
                                .map_err(|error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        11,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                })?,
                            failing_labels: serde_json::from_str(&verification_failures_json)
                                .map_err(|error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        12,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                })?,
                            last_evidence_summary: row.get(13)?,
                            failure_cluster: serde_json::from_str(
                                &verification_failure_cluster_json,
                            )
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    22,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                            requirement_refs: serde_json::from_str(
                                &verification_requirement_refs_json,
                            )
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    23,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                        },
                        completion: CompletionState {
                            closeout_ready: row.get::<_, i64>(14)? != 0,
                            open_work_count: row.get::<_, i64>(15)? as usize,
                            verification_pending: row.get::<_, i64>(16)? != 0,
                            route_contract_pending: row.get::<_, i64>(17)? != 0,
                            blocked_reason: row.get(18)?,
                            route_contract_summary: row.get(19)?,
                        },
                        token_accounting: serde_json::from_str(&token_accounting_json).map_err(
                            |error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    24,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            },
                        )?,
                        docs_route: serde_json::from_str(&docs_route_state_json).map_err(
                            |error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    20,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            },
                        )?,
                        implementation_handoff: serde_json::from_str(&implementation_handoff_json)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    21,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                    })
                },
            )
            .optional()?;

        match row {
            Some(state) => Ok(state),
            None => {
                let state = SessionStateSnapshot::default();
                upsert_session_state_row(&connection, session_id, &state, SystemClock::now_ms())?;
                Ok(state)
            }
        }
    }

    async fn update_todos(
        &self,
        session_id: SessionId,
        todos: &[TodoItem],
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![session_id.to_string()],
        )?;
        for (position, todo) in todos.iter().enumerate() {
            transaction.execute(
                "INSERT INTO session_todos (
                     session_id, todo_id, position, content, kind, status, priority, targets_json,
                     depends_on_json, success_criteria_json, blocked_by_json
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    session_id.to_string(),
                    todo.id.to_string(),
                    position as i64,
                    todo.content,
                    todo_kind_text(todo.kind),
                    todo_status_text(todo.status),
                    todo_priority_text(todo.priority),
                    serde_json::to_string(&todo.targets)?,
                    serde_json::to_string(&todo.depends_on)?,
                    serde_json::to_string(&todo.success_criteria)?,
                    serde_json::to_string(&todo.blocked_by)?
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    async fn list_todos(&self, session_id: SessionId) -> Result<Vec<TodoItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT todo_id, content, kind, status, priority, targets_json, depends_on_json,
                    success_criteria_json, blocked_by_json
             FROM session_todos
             WHERE session_id = ?1
             ORDER BY position ASC",
        )?;
        let todos = statement
            .query_map(params![session_id.to_string()], |row| {
                let todo_id_text: String = row.get(0)?;
                let kind_text: String = row.get(2)?;
                let status_text: String = row.get(3)?;
                let priority_text: String = row.get(4)?;
                let targets_json: String = row.get(5)?;
                let depends_on_json: String = row.get(6)?;
                let success_criteria_json: String = row.get(7)?;
                let blocked_by_json: String = row.get(8)?;
                Ok(TodoItem {
                    id: todo_id_text.parse().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    content: row.get(1)?,
                    kind: parse_todo_kind(&kind_text),
                    status: parse_todo_status(&status_text),
                    priority: parse_todo_priority(&priority_text),
                    targets: serde_json::from_str(&targets_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    depends_on: serde_json::from_str(&depends_on_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    success_criteria: serde_json::from_str(&success_criteria_json).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                7,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    blocked_by: serde_json::from_str(&blocked_by_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(todos)
    }
}

fn parse_status(value: &str) -> SessionStatus {
    match value {
        "idle" => SessionStatus::Idle,
        "running" => SessionStatus::Running,
        "completed" => SessionStatus::Completed,
        "awaiting_user" => SessionStatus::AwaitingUser,
        "cancelled" => SessionStatus::Cancelled,
        "failed" => SessionStatus::Failed,
        _ => SessionStatus::Failed,
    }
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

fn parse_memory_mode(value: &str) -> SessionMemoryMode {
    SessionMemoryMode::parse(value).unwrap_or(SessionMemoryMode::Enabled)
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

fn parse_part_kind(value: &str) -> PartKind {
    match value {
        "text" => PartKind::Text,
        "reasoning" => PartKind::Reasoning,
        "tool_call" => PartKind::ToolCall,
        "tool_result" => PartKind::ToolResult,
        "image" => PartKind::Image,
        "error" => PartKind::Error,
        "diff_summary" => PartKind::DiffSummary,
        "prompt_dispatch" => PartKind::PromptDispatch,
        "request_diagnostics" => PartKind::RequestDiagnostics,
        _ => PartKind::Error,
    }
}

fn part_kind_text(value: PartKind) -> &'static str {
    match value {
        PartKind::Text => "text",
        PartKind::Reasoning => "reasoning",
        PartKind::ToolCall => "tool_call",
        PartKind::ToolResult => "tool_result",
        PartKind::Image => "image",
        PartKind::Error => "error",
        PartKind::DiffSummary => "diff_summary",
        PartKind::PromptDispatch => "prompt_dispatch",
        PartKind::RequestDiagnostics => "request_diagnostics",
    }
}

fn next_message_sequence_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    session_id: SessionId,
) -> Result<i64, StorageError> {
    let value: Option<i64> = transaction.query_row(
        "SELECT MAX(sequence_no) FROM messages WHERE session_id = ?1",
        params![session_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(value.unwrap_or(0) + 1)
}

fn next_part_sequence_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    message_id: MessageId,
) -> Result<i64, StorageError> {
    let value: Option<i64> = transaction.query_row(
        "SELECT MAX(sequence_no) FROM message_parts WHERE message_id = ?1",
        params![message_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(value.unwrap_or(0) + 1)
}

fn insert_part_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    message_id: MessageId,
    part: NewPart,
) -> Result<PartRecord, StorageError> {
    let id = crate::session::PartId::new();
    let now = SystemClock.now_ms();
    let sequence_no = next_part_sequence_in_transaction(transaction, message_id)?;
    transaction.execute(
        "INSERT INTO message_parts (id, message_id, sequence_no, part_kind, payload_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id.to_string(),
            message_id.to_string(),
            sequence_no,
            part_kind_text(part.kind),
            serde_json::to_string(&part.payload)?,
            now
        ],
    )?;
    Ok(PartRecord {
        id,
        message_id,
        sequence_no,
        kind: part.kind,
        payload: part.payload,
    })
}

fn insert_protocol_projection_if_requested(
    transaction: &rusqlite::Transaction<'_>,
    event: &RunEvent,
    fallback_session_id: Option<SessionId>,
    protocol_turn_id: TurnId,
    protocol_sequence_no: Option<i64>,
) -> Result<(), StorageError> {
    let Some(protocol_sequence_no) = protocol_sequence_no else {
        return Ok(());
    };
    let Some(projection) = project_protocol_run_event(
        event,
        fallback_session_id,
        protocol_turn_id,
        protocol_sequence_no,
    ) else {
        return Ok(());
    };
    crate::protocol::insert_event_bundle_in_transaction(
        transaction,
        &projection.runtime_event,
        projection.history_item.as_ref(),
        projection.turn_item.as_ref(),
    )?;
    Ok(())
}

fn tool_success_from_metadata(metadata: &serde_json::Value) -> Option<bool> {
    if let Some(success) = metadata
        .get("success")
        .or_else(|| {
            metadata
                .get("tool_feedback_envelope")
                .and_then(|feedback| feedback.get("success"))
        })
        .and_then(serde_json::Value::as_bool)
    {
        return Some(success);
    }
    if let Some(run) = metadata
        .get("verification_run_result")
        .and_then(|value| serde_json::from_value::<VerificationRunResult>(value.clone()).ok())
    {
        return Some(matches!(run.status, VerificationRunStatus::Passed));
    }
    Some(!matches!(
        tool_progress_effect_from_metadata(metadata),
        ToolProgressEffect::NoProgress
            | ToolProgressEffect::Blocked
            | ToolProgressEffect::VerificationFailed
    ))
}

fn tool_progress_effect_from_metadata(metadata: &serde_json::Value) -> ToolProgressEffect {
    if let Some(run) = metadata
        .get("verification_run_result")
        .and_then(|value| serde_json::from_value::<VerificationRunResult>(value.clone()).ok())
    {
        return match run.status {
            VerificationRunStatus::Passed => ToolProgressEffect::VerificationPassed,
            VerificationRunStatus::Failed | VerificationRunStatus::TimedOut => {
                ToolProgressEffect::VerificationFailed
            }
            VerificationRunStatus::NotVerification => ToolProgressEffect::Unknown,
        };
    }
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("progress_effect"))
        .or_else(|| metadata.get("progress_effect"))
        .and_then(serde_json::Value::as_str)
        .map(|value| match value {
            "made_progress" | "progress" => ToolProgressEffect::MadeProgress,
            "no_progress" => ToolProgressEffect::NoProgress,
            "blocked" => ToolProgressEffect::Blocked,
            "verification_passed" => ToolProgressEffect::VerificationPassed,
            "verification_failed" => ToolProgressEffect::VerificationFailed,
            _ => ToolProgressEffect::Unknown,
        })
        .unwrap_or(ToolProgressEffect::Unknown)
}

fn metadata_string(metadata: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut value = metadata;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_str().map(ToString::to_string)
}

fn upsert_session_state_row(
    connection: &Connection,
    session_id: SessionId,
    state: &SessionStateSnapshot,
    updated_at_ms: i64,
) -> Result<(), StorageError> {
    let failure_kind = state
        .failure
        .as_ref()
        .map(|value| failure_kind_text(value.kind));
    let failure_summary = state.failure.as_ref().map(|value| value.summary.as_str());
    let failure_tool_name = state
        .failure
        .as_ref()
        .and_then(|value| value.tool_name)
        .map(tool_name_text);
    let failure_targets_json = serde_json::to_string(
        &state
            .failure
            .as_ref()
            .map(|value| value.targets.as_slice())
            .unwrap_or(&[]),
    )?;
    let contract_refs_json = serde_json::to_string(&state.contract_refs)?;
    let review_scope_json = serde_json::to_string(&state.review_scope)?;
    let docs_route_state_json = serde_json::to_string(&state.docs_route)?;
    let implementation_handoff_json = serde_json::to_string(&state.implementation_handoff)?;
    let verification_failure_cluster_json =
        serde_json::to_string(&state.verification.failure_cluster)?;
    let verification_requirement_refs_json =
        serde_json::to_string(&state.verification.requirement_refs)?;
    let token_accounting_json = serde_json::to_string(&state.token_accounting)?;
    connection.execute(
        "INSERT INTO session_state (
             session_id, task_route, phase, review_scope_json, active_todo_id, active_targets_json, contract_refs_json, failure_kind, failure_summary, failure_tool_name,
             failure_targets_json, verification_todo_id, verification_commands_json, verification_failures_json,
             verification_evidence_summary, completion_closeout_ready, completion_open_work_count,
             completion_verification_pending, completion_route_contract_pending, completion_blocked_reason, completion_route_contract_summary,
             docs_route_state_json, implementation_handoff_json, verification_failure_cluster_json, verification_requirement_refs_json, token_accounting_json, updated_at_ms
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
         ON CONFLICT(session_id) DO UPDATE SET
             task_route = excluded.task_route,
             phase = excluded.phase,
             review_scope_json = excluded.review_scope_json,
             active_todo_id = excluded.active_todo_id,
             active_targets_json = excluded.active_targets_json,
             contract_refs_json = excluded.contract_refs_json,
             failure_kind = excluded.failure_kind,
             failure_summary = excluded.failure_summary,
             failure_tool_name = excluded.failure_tool_name,
             failure_targets_json = excluded.failure_targets_json,
             verification_todo_id = excluded.verification_todo_id,
             verification_commands_json = excluded.verification_commands_json,
             verification_failures_json = excluded.verification_failures_json,
             verification_evidence_summary = excluded.verification_evidence_summary,
             completion_closeout_ready = excluded.completion_closeout_ready,
             completion_open_work_count = excluded.completion_open_work_count,
             completion_verification_pending = excluded.completion_verification_pending,
             completion_route_contract_pending = excluded.completion_route_contract_pending,
             completion_blocked_reason = excluded.completion_blocked_reason,
             completion_route_contract_summary = excluded.completion_route_contract_summary,
             docs_route_state_json = excluded.docs_route_state_json,
             implementation_handoff_json = excluded.implementation_handoff_json,
             verification_failure_cluster_json = excluded.verification_failure_cluster_json,
             verification_requirement_refs_json = excluded.verification_requirement_refs_json,
             token_accounting_json = excluded.token_accounting_json,
             updated_at_ms = excluded.updated_at_ms",
        params![
            session_id.to_string(),
            task_route_text(state.route),
            process_phase_text(state.process_phase),
            review_scope_json,
            state.active_todo_id.map(|value| value.to_string()),
            serde_json::to_string(&state.active_targets)?,
            contract_refs_json,
            failure_kind,
            failure_summary,
            failure_tool_name,
            failure_targets_json,
            state.verification.pending_todo_id.map(|value| value.to_string()),
            serde_json::to_string(&state.verification.required_commands)?,
            serde_json::to_string(&state.verification.failing_labels)?,
            state.verification.last_evidence_summary,
            state.completion.closeout_ready as i64,
            state.completion.open_work_count as i64,
            state.completion.verification_pending as i64,
            state.completion.route_contract_pending as i64,
            state.completion.blocked_reason,
            state.completion.route_contract_summary,
            docs_route_state_json,
            implementation_handoff_json,
            verification_failure_cluster_json,
            verification_requirement_refs_json,
            token_accounting_json,
            updated_at_ms
        ],
    )?;
    Ok(())
}

fn validate_file_change_protocol_bundle(
    tool_call_id: ToolCallId,
    diff_summary: &DiffSummaryPart,
    file_changes: &[crate::edit::ChangeSummary],
) -> Result<(), StorageError> {
    if file_changes.is_empty() {
        return Err(StorageError::Message(
            "content-changing tool completion requires file change evidence".to_string(),
        ));
    }
    if diff_summary.summary.trim().is_empty() {
        return Err(StorageError::Message(
            "content-changing tool completion requires a diff summary".to_string(),
        ));
    }
    if diff_summary.tool_call_id != Some(tool_call_id) {
        return Err(StorageError::Message(
            "diff summary tool call id must match tool completion owner".to_string(),
        ));
    }
    let change_ids = file_changes
        .iter()
        .map(|change| change.change_id)
        .collect::<Vec<_>>();
    if diff_summary.change_ids != change_ids {
        return Err(StorageError::Message(
            "diff summary change ids must match file change evidence".to_string(),
        ));
    }
    if diff_summary.changes.len() != file_changes.len() {
        return Err(StorageError::Message(
            "diff summary evidence count must match file change evidence".to_string(),
        ));
    }
    for (index, (evidence, change)) in diff_summary
        .changes
        .iter()
        .zip(file_changes.iter())
        .enumerate()
    {
        if evidence.change_id != change.change_id
            || evidence.kind != change.kind
            || evidence.path_before != change.path_before
            || evidence.path_after != change.path_after
        {
            return Err(StorageError::Message(format!(
                "diff summary evidence at index {index} must match file change evidence"
            )));
        }
        if evidence.summary.trim().is_empty() {
            return Err(StorageError::Message(format!(
                "diff summary evidence at index {index} requires a summary"
            )));
        }
    }
    Ok(())
}

fn parse_tool_name(value: &str) -> crate::tool::ToolName {
    match value {
        "list" => crate::tool::ToolName::List,
        "glob" => crate::tool::ToolName::Glob,
        "grep" => crate::tool::ToolName::Grep,
        "read" => crate::tool::ToolName::Read,
        "inspect_directory" => crate::tool::ToolName::InspectDirectory,
        "apply_patch" => crate::tool::ToolName::ApplyPatch,
        "write" => crate::tool::ToolName::Write,
        "shell" => crate::tool::ToolName::Shell,
        "current_time" => crate::tool::ToolName::CurrentTime,
        "skill" => crate::tool::ToolName::Skill,
        "docling_convert" => crate::tool::ToolName::DoclingConvert,
        "mcp_call" => crate::tool::ToolName::McpCall,
        "todowrite" => crate::tool::ToolName::TodoWrite,
        "get_goal" => crate::tool::ToolName::GetGoal,
        "create_goal" => crate::tool::ToolName::CreateGoal,
        "update_goal" => crate::tool::ToolName::UpdateGoal,
        "invalid" => crate::tool::ToolName::Invalid,
        _ => crate::tool::ToolName::Invalid,
    }
}

fn tool_name_text(value: crate::tool::ToolName) -> &'static str {
    match value {
        crate::tool::ToolName::List => "list",
        crate::tool::ToolName::Glob => "glob",
        crate::tool::ToolName::Grep => "grep",
        crate::tool::ToolName::Read => "read",
        crate::tool::ToolName::InspectDirectory => "inspect_directory",
        crate::tool::ToolName::ApplyPatch => "apply_patch",
        crate::tool::ToolName::Write => "write",
        crate::tool::ToolName::Shell => "shell",
        crate::tool::ToolName::CurrentTime => "current_time",
        crate::tool::ToolName::Skill => "skill",
        crate::tool::ToolName::DoclingConvert => "docling_convert",
        crate::tool::ToolName::McpCall => "mcp_call",
        crate::tool::ToolName::TodoWrite => "todowrite",
        crate::tool::ToolName::GetGoal => "get_goal",
        crate::tool::ToolName::CreateGoal => "create_goal",
        crate::tool::ToolName::UpdateGoal => "update_goal",
        crate::tool::ToolName::Invalid => "invalid",
    }
}

fn parse_task_route(value: &str) -> Result<TaskRoute, rusqlite::Error> {
    match value {
        "code" => Ok(TaskRoute::Code),
        "docs" => Ok(TaskRoute::Docs),
        "review" => Ok(TaskRoute::Review),
        "debug" => Ok(TaskRoute::Debug),
        "ask" => Ok(TaskRoute::Ask),
        "summary" => Ok(TaskRoute::Summary),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::<dyn std::error::Error + Send + Sync>::from(format!(
                "invalid task route `{value}`"
            )),
        )),
    }
}

fn task_route_text(value: TaskRoute) -> &'static str {
    match value {
        TaskRoute::Code => "code",
        TaskRoute::Docs => "docs",
        TaskRoute::Review => "review",
        TaskRoute::Debug => "debug",
        TaskRoute::Ask => "ask",
        TaskRoute::Summary => "summary",
    }
}

fn parse_process_phase(value: &str) -> Result<ProcessPhase, rusqlite::Error> {
    match value {
        "discovery" | "planning" => Ok(ProcessPhase::Discover),
        "editing" => Ok(ProcessPhase::Author),
        "verifying" => Ok(ProcessPhase::Verify),
        "repairing" => Ok(ProcessPhase::Repair),
        "completing" => Ok(ProcessPhase::Closeout),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::<dyn std::error::Error + Send + Sync>::from(format!(
                "invalid process phase `{value}`"
            )),
        )),
    }
}

fn process_phase_text(value: ProcessPhase) -> &'static str {
    match value {
        ProcessPhase::Discover => "discovery",
        ProcessPhase::Author => "editing",
        ProcessPhase::Verify => "verifying",
        ProcessPhase::Repair => "repairing",
        ProcessPhase::Closeout => "completing",
    }
}

fn parse_failure_kind(value: &str) -> Result<FailureKind, rusqlite::Error> {
    match value {
        "invalid_tool" => Ok(FailureKind::InvalidTool),
        "tool_execution" => Ok(FailureKind::ToolExecution),
        "patch_mismatch" => Ok(FailureKind::PatchMismatch),
        "verification_failed" => Ok(FailureKind::VerificationFailed),
        "context_overflow" => Ok(FailureKind::ContextOverflow),
        "provider_retryable" => Ok(FailureKind::ProviderRetryable),
        "provider_fatal" => Ok(FailureKind::ProviderFatal),
        "completion_drift" => Ok(FailureKind::CompletionDrift),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::<dyn std::error::Error + Send + Sync>::from(format!(
                "invalid failure kind `{value}`"
            )),
        )),
    }
}

fn failure_kind_text(value: FailureKind) -> &'static str {
    match value {
        FailureKind::InvalidTool => "invalid_tool",
        FailureKind::ToolExecution => "tool_execution",
        FailureKind::PatchMismatch => "patch_mismatch",
        FailureKind::VerificationFailed => "verification_failed",
        FailureKind::ContextOverflow => "context_overflow",
        FailureKind::ProviderRetryable => "provider_retryable",
        FailureKind::ProviderFatal => "provider_fatal",
        FailureKind::CompletionDrift => "completion_drift",
    }
}

fn parse_todo_status(value: &str) -> TodoStatus {
    match value {
        "pending" => TodoStatus::Pending,
        "in_progress" => TodoStatus::InProgress,
        "blocked" => TodoStatus::Blocked,
        "completed" => TodoStatus::Completed,
        "cancelled" => TodoStatus::Cancelled,
        _ => TodoStatus::Pending,
    }
}

fn todo_status_text(value: TodoStatus) -> &'static str {
    match value {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Blocked => "blocked",
        TodoStatus::Completed => "completed",
        TodoStatus::Cancelled => "cancelled",
    }
}

fn parse_todo_kind(value: &str) -> TodoKind {
    match value {
        "verification" => TodoKind::Verification,
        "repair" => TodoKind::Repair,
        "completion" => TodoKind::Completion,
        _ => TodoKind::Work,
    }
}

fn todo_kind_text(value: TodoKind) -> &'static str {
    match value {
        TodoKind::Work => "work",
        TodoKind::Verification => "verification",
        TodoKind::Repair => "repair",
        TodoKind::Completion => "completion",
    }
}

fn parse_todo_priority(value: &str) -> TodoPriority {
    match value {
        "high" => TodoPriority::High,
        "medium" => TodoPriority::Medium,
        "low" => TodoPriority::Low,
        _ => TodoPriority::Medium,
    }
}

fn todo_priority_text(value: TodoPriority) -> &'static str {
    match value {
        TodoPriority::High => "high",
        TodoPriority::Medium => "medium",
        TodoPriority::Low => "low",
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::config::AccessMode;
    use crate::protocol::{ContentPart, ProtocolEventStore, UserInputItem};
    use crate::session::{
        AssistantMessageMeta, FinishReason, NewSession, ProjectId, ProjectRepository,
        SessionRepository,
    };
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    async fn test_repo() -> (StoreBundle, SessionId) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
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

    async fn active_turn_fixture() -> (StoreBundle, SessionId, String, TurnId, MessageRecord) {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let admission_id = repo
            .admit_session_run(session_id)
            .await
            .expect("admit run")
            .expect("run admitted");
        let turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );
        store
            .protocol_event_store()
            .append_history_item(&HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                turn_id,
                sequence_no: 0,
                created_at_ms: SystemClock.now_ms(),
                payload: HistoryItemPayload::Message {
                    message_id: None,
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: "initial request".to_string(),
                    }],
                },
            })
            .expect("record active turn");
        let (assistant, _) = repo
            .append_assistant_message_with_protocol_start(
                NewMessage {
                    session_id,
                    parent_message_id: None,
                    role: MessageRole::Assistant,
                    metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                        model: "model".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                &admission_id,
                turn_id,
                None,
                "model".to_string(),
            )
            .await
            .expect("assistant");
        (store, session_id, admission_id, turn_id, assistant)
    }

    fn test_assistant_metadata(finish_reason: Option<FinishReason>) -> MessageMetadata {
        MessageMetadata::Assistant(AssistantMessageMeta {
            model: "model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            finish_reason,
            token_usage: None,
            summary: false,
        })
    }

    fn stored_run_lease_expiry(repo: &SqliteSessionRepository, session_id: SessionId) -> i64 {
        repo.connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT active_run_lease_expires_at_ms FROM sessions WHERE id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("stored run lease expiry")
    }

    #[tokio::test]
    async fn heartbeat_extends_lease_without_shortening_on_clock_rollback() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let turn_id = TurnId::new();
        let admission_id = repo
            .admit_session_run_at(session_id, 1_000, 100)
            .await
            .expect("admit")
            .expect("admitted");
        assert_eq!(stored_run_lease_expiry(&repo, session_id), 1_100);
        assert_eq!(
            repo.renew_admitted_run_lease_at(session_id, &admission_id, turn_id, 1_050, 100,)
                .await
                .expect("heartbeat"),
            RunAdmissionLeaseRenewalOutcome::Renewed
        );
        assert_eq!(stored_run_lease_expiry(&repo, session_id), 1_150);
        assert_eq!(
            repo.renew_admitted_run_lease_at(session_id, &admission_id, turn_id, 900, 100,)
                .await
                .expect("rollback heartbeat"),
            RunAdmissionLeaseRenewalOutcome::Renewed
        );
        assert_eq!(stored_run_lease_expiry(&repo, session_id), 1_150);
        assert!(
            repo.admit_session_run_at(session_id, 1_149, 100)
                .await
                .expect("pre-expiry admission")
                .is_none()
        );
        assert!(
            repo.admit_session_run_at(session_id, 1_150, 100)
                .await
                .expect("post-expiry admission")
                .is_some()
        );
    }

    #[tokio::test]
    async fn terminal_owner_clear_is_a_graceful_heartbeat_outcome() {
        let (store, session_id, admission_id, turn_id, assistant) = active_turn_fixture().await;
        let repo = store.session_repo();
        assert_eq!(
            repo.update_admitted_message_metadata_and_status_with_protocol_event(
                session_id,
                &admission_id,
                assistant.id,
                &test_assistant_metadata(Some(FinishReason::Stop)),
                SessionStatus::Completed,
                &RunEvent::SessionCompleted {
                    session_id,
                    finish_reason: Some(FinishReason::Stop),
                },
                turn_id,
                None,
                None,
                None,
            )
            .await
            .expect("terminal commit"),
            AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            repo.renew_admitted_run_lease(session_id, &admission_id, turn_id)
                .await
                .expect("post-terminal heartbeat"),
            RunAdmissionLeaseRenewalOutcome::GracefulTerminal
        );
    }

    #[tokio::test]
    async fn lease_clock_edges_are_clamped_and_overflow_safe() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let first_admission = repo
            .admit_session_run_at(session_id, -10, 0)
            .await
            .expect("negative clock admission")
            .expect("admitted");
        assert_eq!(stored_run_lease_expiry(&repo, session_id), 1);
        assert!(
            repo.has_fresh_run_admission_at(session_id, 0)
                .await
                .expect("fresh at clamped zero")
        );
        assert!(
            !repo
                .has_fresh_run_admission_at(session_id, 1)
                .await
                .expect("expired at exact boundary")
        );
        assert_eq!(
            repo.renew_admitted_run_lease_at(session_id, &first_admission, TurnId::new(), 1, 100,)
                .await
                .expect("expired heartbeat rejected"),
            RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
        );
        repo.admit_session_run_at(session_id, i64::MAX, i64::MAX)
            .await
            .expect("overflow-safe reclaim")
            .expect("reclaimed");
        assert_eq!(stored_run_lease_expiry(&repo, session_id), i64::MAX);
        assert!(
            repo.has_fresh_run_admission_at(session_id, i64::MAX)
                .await
                .expect("saturated lease remains fresh")
        );
    }

    #[tokio::test]
    async fn expired_owner_recovery_covers_active_and_terminal_statuses() {
        for expired_status in [
            SessionStatus::Running,
            SessionStatus::AwaitingUser,
            SessionStatus::Cancelled,
            SessionStatus::Failed,
        ] {
            let (store, session_id, admission_id, turn_id, assistant) = active_turn_fixture().await;
            let repo = store.session_repo();
            let tool_call = repo
                .insert_tool_call(
                    session_id,
                    assistant.id,
                    "shell",
                    "{}",
                    Some("crashed tool"),
                    serde_json::Value::Null,
                )
                .await
                .expect("pending tool");
            match expired_status {
                SessionStatus::Running => {}
                SessionStatus::AwaitingUser => {
                    assert!(
                        repo.terminalize_admitted_session_with_protocol_event(
                            session_id,
                            &admission_id,
                            SessionStatus::AwaitingUser,
                            &RunEvent::SessionAwaitingUser {
                                session_id,
                                finish_reason: Some(FinishReason::ToolCall),
                            },
                            turn_id,
                            None,
                        )
                        .await
                        .expect("awaiting user")
                    );
                }
                SessionStatus::Cancelled => {
                    assert!(
                        repo.terminalize_active_session_with_protocol_event(
                            session_id,
                            SessionStatus::Cancelled,
                            &RunEvent::SessionInterrupted {
                                session_id,
                                reason: "external cancellation".to_string(),
                            },
                            turn_id,
                            None,
                        )
                        .await
                        .expect("external cancellation")
                    );
                }
                SessionStatus::Failed => {
                    assert!(
                        repo.terminalize_active_session_with_protocol_event(
                            session_id,
                            SessionStatus::Failed,
                            &RunEvent::SessionFailed {
                                session_id,
                                message: "external failure".to_string(),
                            },
                            turn_id,
                            None,
                        )
                        .await
                        .expect("external failure")
                    );
                }
                SessionStatus::Idle | SessionStatus::Completed => unreachable!(),
            }
            let lease_expiry = stored_run_lease_expiry(&repo, session_id);
            assert!(
                repo.admit_session_run_at(
                    session_id,
                    lease_expiry.saturating_sub(1),
                    RUN_ADMISSION_LEASE_DURATION_MS,
                )
                .await
                .expect("pre-expiry admission")
                .is_none(),
                "fresh {expired_status:?} owner must not be reclaimed"
            );
            let replacement = repo
                .admit_session_run_at(session_id, lease_expiry, RUN_ADMISSION_LEASE_DURATION_MS)
                .await
                .expect("expired owner recovery")
                .expect("replacement admission");
            assert_ne!(replacement, admission_id);
            assert_eq!(
                repo.get_session(session_id)
                    .await
                    .expect("recovered session")
                    .status,
                SessionStatus::Running
            );
            let tool_status = repo
                .connection
                .lock()
                .expect("sqlite mutex")
                .query_row(
                    "SELECT status FROM tool_calls WHERE id = ?1",
                    params![tool_call.id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .expect("tool status");
            assert_eq!(tool_status, "failed");
            let expired_interrupts = store
                .protocol_event_store()
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events")
                .into_iter()
                .filter(|event| {
                    matches!(
                        &event.msg,
                        RuntimeEventMsg::TurnInterrupted { reason }
                            if reason == EXPIRED_RUN_RECOVERY_REASON
                    )
                })
                .count();
            let expected_expired_interrupts = usize::from(matches!(
                expired_status,
                SessionStatus::Running | SessionStatus::AwaitingUser
            ));
            assert_eq!(expired_interrupts, expected_expired_interrupts);
        }
    }

    #[tokio::test]
    async fn pre_turn_expiry_does_not_attribute_recovery_to_the_previous_turn() {
        let (store, session_id) = test_repo().await;
        let previous_turn_id = TurnId::new();
        let previous_event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id: previous_turn_id,
            sequence_no: 0,
            created_at_ms: 900,
            msg: RuntimeEventMsg::Warning {
                message: "previous turn is complete".to_string(),
            },
        };
        store
            .protocol_event_store()
            .append_runtime_event(&previous_event)
            .expect("previous turn event");
        let repo = store.session_repo();
        let crashed_admission = repo
            .admit_session_run_at(session_id, 1_000, 100)
            .await
            .expect("crashed admission")
            .expect("admitted before publishing a turn");
        let replacement = repo
            .admit_session_run_at(session_id, 1_100, 100)
            .await
            .expect("expired recovery")
            .expect("replacement admission");

        assert_ne!(replacement, crashed_admission);
        let previous_turn_events = store
            .protocol_event_store()
            .list_runtime_events(session_id, previous_turn_id)
            .expect("previous turn events");
        assert_eq!(previous_turn_events.len(), 1);
        assert_eq!(previous_turn_events[0].id, previous_event.id);
        assert!(matches!(
            &previous_turn_events[0].msg,
            RuntimeEventMsg::Warning { message } if message == "previous turn is complete"
        ));
    }

    #[tokio::test]
    async fn expired_lease_fences_owner_reads_heartbeats_and_protocol_writes() {
        let (store, session_id, admission_id, turn_id, _) = active_turn_fixture().await;
        let repo = store.session_repo();
        let lease_expiry = stored_run_lease_expiry(&repo, session_id);
        assert_eq!(
            repo.admitted_run_status_at(session_id, &admission_id, turn_id, lease_expiry)
                .await
                .expect("status guard"),
            None
        );
        assert!(
            repo.list_admitted_turn_steers_at(session_id, &admission_id, turn_id, lease_expiry,)
                .await
                .expect("mailbox guard")
                .is_none()
        );
        assert_eq!(
            repo.renew_admitted_run_lease_at(
                session_id,
                &admission_id,
                turn_id,
                lease_expiry,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect("heartbeat guard"),
            RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
        );
        let late_event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: lease_expiry,
            msg: RuntimeEventMsg::Warning {
                message: "late expired owner write".to_string(),
            },
        };
        assert!(
            store
                .protocol_event_store()
                .append_admitted_event_bundle_allocating_at(
                    &admission_id,
                    &late_event,
                    None,
                    None,
                    lease_expiry,
                )
                .expect("protocol write guard")
                .is_none()
        );
    }

    #[tokio::test]
    async fn deleting_session_removes_protocol_allocator_rows() {
        let (store, session_id) = test_repo().await;
        let turn_id = TurnId::new();
        store
            .protocol_event_store()
            .append_runtime_event(&RuntimeEvent {
                id: RuntimeEventId::new(),
                session_id,
                turn_id,
                sequence_no: 0,
                created_at_ms: SystemClock.now_ms(),
                msg: RuntimeEventMsg::Warning {
                    message: "delete allocator".to_string(),
                },
            })
            .expect("event");
        let repo = store.session_repo();
        repo.delete_session(session_id)
            .await
            .expect("delete session");
        let allocator_rows = repo
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*) FROM protocol_turn_sequence_allocators WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("allocator count");
        assert_eq!(allocator_rows, 0);
    }

    #[tokio::test]
    async fn thread_goal_insert_replaces_only_completed_goal() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();

        let first = repo
            .insert_thread_goal(
                session_id,
                "ship the goal",
                ThreadGoalStatus::Active,
                Some(100),
            )
            .await
            .expect("insert first")
            .expect("first goal");
        assert_eq!(first.status, ThreadGoalStatus::Active);
        assert_eq!(first.token_budget, Some(100));

        let refused = repo
            .insert_thread_goal(
                session_id,
                "replace too early",
                ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("insert unfinished");
        assert!(refused.is_none());

        repo.update_thread_goal(session_id, None, Some(ThreadGoalStatus::Complete), None)
            .await
            .expect("complete")
            .expect("completed goal");
        let replaced = repo
            .insert_thread_goal(session_id, "next goal", ThreadGoalStatus::Active, None)
            .await
            .expect("insert next")
            .expect("next goal");
        assert_eq!(replaced.objective, "next goal");
        assert_eq!(replaced.tokens_used, 0);
        assert_eq!(replaced.token_budget, None);
    }

    #[tokio::test]
    async fn thread_goal_accounting_marks_budget_limited() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        repo.replace_thread_goal(
            session_id,
            "stay within budget",
            ThreadGoalStatus::Active,
            Some(5),
        )
        .await
        .expect("replace");

        let updated = repo
            .account_thread_goal_usage(session_id, 7)
            .await
            .expect("account")
            .expect("goal");

        assert_eq!(updated.tokens_used, 7);
        assert_eq!(updated.status, ThreadGoalStatus::BudgetLimited);
    }

    #[tokio::test]
    async fn thread_goal_expected_id_prevents_stale_accounting_and_update() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        repo.replace_thread_goal(session_id, "first", ThreadGoalStatus::Active, Some(100))
            .await
            .expect("first");
        let (_first_goal, first_goal_id) = repo
            .get_thread_goal_with_id(session_id)
            .await
            .expect("first id")
            .expect("first goal");

        repo.replace_thread_goal(session_id, "second", ThreadGoalStatus::Active, Some(100))
            .await
            .expect("second");
        repo.account_thread_goal_usage_for_goal(session_id, 10, Some(first_goal_id.as_str()))
            .await
            .expect("stale account");
        repo.update_thread_goal_for_goal(
            session_id,
            None,
            Some(ThreadGoalStatus::Blocked),
            None,
            Some(first_goal_id.as_str()),
        )
        .await
        .expect("stale update");

        let current = repo
            .get_thread_goal(session_id)
            .await
            .expect("current")
            .expect("goal");
        assert_eq!(current.objective, "second");
        assert_eq!(current.status, ThreadGoalStatus::Active);
        assert_eq!(current.tokens_used, 0);
    }

    #[tokio::test]
    async fn session_search_treats_like_wildcards_as_literal_text() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let session = repo.get_session(session_id).await.expect("session");
        repo.update_session_title(session_id, "100%_literal")
            .await
            .expect("title");
        repo.create_session(NewSession {
            project_id: session.project_id,
            title: "plain title".to_string(),
            cwd: session.cwd,
            model: session.model,
            base_url: session.base_url,
            access_mode: session.access_mode,
        })
        .await
        .expect("plain session");

        let matches = repo
            .search_sessions(session.project_id, "%_", 10, true)
            .await
            .expect("search");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, session_id);
    }

    #[test]
    fn concurrent_goal_update_and_usage_accounting_do_not_lose_each_other() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (store, session_id) = runtime.block_on(test_repo());
        runtime
            .block_on(store.session_repo().replace_thread_goal(
                session_id,
                "initial objective",
                ThreadGoalStatus::Active,
                Some(100),
            ))
            .expect("goal");
        let initial_updated_at = store
            .session_repo()
            .get_stored_thread_goal(session_id)
            .expect("stored goal")
            .expect("goal")
            .updated_at_ms;
        let paths = store.paths().clone();
        drop(store);
        let barrier = Arc::new(Barrier::new(3));
        let final_paths = paths.clone();
        let update_paths = paths.clone();
        let update_barrier = Arc::clone(&barrier);
        let update = std::thread::spawn(move || {
            let store = SqliteStore::open(&update_paths).expect("update store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("update runtime");
            update_barrier.wait();
            runtime
                .block_on(store.session_repo().update_thread_goal(
                    session_id,
                    Some("updated objective"),
                    None,
                    None,
                ))
                .expect("update goal");
        });
        let usage_barrier = Arc::clone(&barrier);
        let usage = std::thread::spawn(move || {
            let store = SqliteStore::open(&paths).expect("usage store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("usage runtime");
            usage_barrier.wait();
            runtime
                .block_on(
                    store
                        .session_repo()
                        .account_thread_goal_usage(session_id, 7),
                )
                .expect("account usage");
        });
        barrier.wait();
        update.join().expect("update thread");
        usage.join().expect("usage thread");
        let store = SqliteStore::open(&final_paths).expect("final store");
        let stored = store
            .session_repo()
            .get_stored_thread_goal(session_id)
            .expect("final goal")
            .expect("goal");

        assert_eq!(stored.goal.objective, "updated objective");
        assert_eq!(stored.goal.tokens_used, 7);
        assert!(stored.updated_at_ms >= initial_updated_at.saturating_add(2));
    }

    #[test]
    fn concurrent_disjoint_session_settings_updates_are_merged() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (session_id, initial_updated_at) = runtime.block_on(async {
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
                    title: "settings race".to_string(),
                    cwd: data_dir,
                    model: "initial-model".to_string(),
                    base_url: "http://initial".to_string(),
                    access_mode: AccessMode::Default,
                })
                .await
                .expect("session");
            (session.id, session.updated_at_ms)
        });
        drop(store);
        let barrier = Arc::new(Barrier::new(3));
        let final_paths = paths.clone();
        let model_paths = paths.clone();
        let model_barrier = Arc::clone(&barrier);
        let model_update = std::thread::spawn(move || {
            let store = SqliteStore::open(&model_paths).expect("model store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("model runtime");
            model_barrier.wait();
            runtime
                .block_on(store.session_repo().update_session_settings(
                    session_id,
                    &SessionSettingsPatch {
                        model: Some("updated-model".to_string()),
                        ..SessionSettingsPatch::default()
                    },
                ))
                .expect("model update");
        });
        let base_barrier = Arc::clone(&barrier);
        let base_update = std::thread::spawn(move || {
            let store = SqliteStore::open(&paths).expect("base store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("base runtime");
            base_barrier.wait();
            runtime
                .block_on(store.session_repo().update_session_settings(
                    session_id,
                    &SessionSettingsPatch {
                        base_url: Some("http://updated".to_string()),
                        ..SessionSettingsPatch::default()
                    },
                ))
                .expect("base update");
        });
        barrier.wait();
        model_update.join().expect("model thread");
        base_update.join().expect("base thread");
        let store = SqliteStore::open(&final_paths).expect("final store");
        let final_session = runtime
            .block_on(store.session_repo().get_session(session_id))
            .expect("final session");

        assert_eq!(final_session.model, "updated-model");
        assert_eq!(final_session.base_url, "http://updated");
        assert!(final_session.updated_at_ms >= initial_updated_at.saturating_add(2));
    }

    #[test]
    fn concurrent_database_admission_allows_one_run_across_connections() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let setup = StoreBundle::new(sqlite);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let session_id = runtime.block_on(async {
            let project_id = ProjectId::new();
            setup
                .project_repo()
                .upsert_project(project_id, &data_dir, "test", "none")
                .await
                .expect("project");
            setup
                .session_repo()
                .create_session(NewSession {
                    project_id,
                    title: "race".to_string(),
                    cwd: data_dir,
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                    access_mode: AccessMode::Default,
                })
                .await
                .expect("session")
                .id
        });
        drop(setup);
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let paths = paths.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let store = SqliteStore::open(&paths).expect("worker store");
                let repo = store.session_repo();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("worker runtime");
                barrier.wait();
                runtime
                    .block_on(repo.try_admit_session_run(session_id))
                    .expect("admission")
            }));
        }
        barrier.wait();
        let admitted = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .filter(|admitted| *admitted)
            .count();

        assert_eq!(admitted, 1);
    }

    #[test]
    fn lease_reclaim_is_linearized_across_connections() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (store, session_id) = runtime.block_on(test_repo());
        let paths = store.paths().clone();
        let repo = store.session_repo();
        runtime
            .block_on(repo.admit_session_run_at(session_id, 10_000, 100))
            .expect("initial admission")
            .expect("initial owner");

        let fresh_barrier = Arc::new(Barrier::new(3));
        let mut fresh_workers = Vec::new();
        for _ in 0..2 {
            let paths = paths.clone();
            let barrier = Arc::clone(&fresh_barrier);
            fresh_workers.push(std::thread::spawn(move || {
                let store = SqliteStore::open(&paths).expect("fresh worker store");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("fresh worker runtime");
                barrier.wait();
                runtime
                    .block_on(
                        store
                            .session_repo()
                            .admit_session_run_at(session_id, 10_099, 100),
                    )
                    .expect("fresh admission result")
            }));
        }
        fresh_barrier.wait();
        assert!(
            fresh_workers
                .into_iter()
                .all(|worker| worker.join().expect("fresh worker").is_none())
        );

        let expired_barrier = Arc::new(Barrier::new(3));
        let mut expired_workers = Vec::new();
        for _ in 0..2 {
            let paths = paths.clone();
            let barrier = Arc::clone(&expired_barrier);
            expired_workers.push(std::thread::spawn(move || {
                let store = SqliteStore::open(&paths).expect("expired worker store");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("expired worker runtime");
                barrier.wait();
                runtime
                    .block_on(
                        store
                            .session_repo()
                            .admit_session_run_at(session_id, 10_100, 100),
                    )
                    .expect("expired admission result")
            }));
        }
        expired_barrier.wait();
        let reclaimed = expired_workers
            .into_iter()
            .map(|worker| worker.join().expect("expired worker"))
            .filter(Option::is_some)
            .count();
        assert_eq!(reclaimed, 1);
    }

    #[test]
    fn live_process_lock_blocks_reclaim_even_after_database_lease_expiry() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (store, session_id) = runtime.block_on(test_repo());
        let paths = store.paths().clone();
        let live_process_guard = store
            .try_acquire_run_process_lease(session_id)
            .expect("live process lock");
        runtime
            .block_on(
                store
                    .session_repo()
                    .admit_session_run_at(session_id, 5_000, 100),
            )
            .expect("database admission")
            .expect("database owner");
        let other_process = StoreBundle::new(SqliteStore::open(&paths).expect("other store"));

        assert!(
            other_process
                .try_acquire_run_process_lease(session_id)
                .is_err()
        );
        drop(live_process_guard);
        let _replacement_process_guard = other_process
            .try_acquire_run_process_lease(session_id)
            .expect("lock released after owner exit");
        assert!(
            runtime
                .block_on(
                    other_process
                        .session_repo()
                        .admit_session_run_at(session_id, 5_100, 100),
                )
                .expect("expired database reclaim")
                .is_some()
        );
    }

    #[tokio::test]
    async fn completion_waits_for_an_accepted_unseen_steer() {
        let (store, session_id, admission_id, turn_id, assistant) = active_turn_fixture().await;
        let repo = store.session_repo();
        let steer = SteerTurn {
            expected_turn_id: turn_id,
            items: vec![UserInputItem::Text {
                text: "change the final answer".to_string(),
            }],
            additional_context: Default::default(),
            client_user_message_id: Some("accepted-before-complete".to_string()),
        };
        repo.accept_active_turn_steer(session_id, &steer)
            .await
            .expect("accept steer");
        let completed = RunEvent::SessionCompleted {
            session_id,
            finish_reason: Some(FinishReason::Stop),
        };
        let metadata = MessageMetadata::Assistant(AssistantMessageMeta {
            model: "model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            finish_reason: Some(FinishReason::Stop),
            token_usage: None,
            summary: false,
        });

        assert_eq!(
            repo.update_admitted_message_metadata_and_status_with_protocol_event(
                session_id,
                &admission_id,
                assistant.id,
                &metadata,
                SessionStatus::Completed,
                &completed,
                turn_id,
                Some(1),
                Some(0),
                None,
            )
            .await
            .expect("reject completion before steer drain"),
            AdmittedTerminalCommit::UnseenSteer {
                expected: 0,
                actual: 1,
            }
        );
        assert_eq!(
            repo.get_session(session_id).await.expect("running").status,
            SessionStatus::Running
        );
        assert_eq!(
            repo.update_admitted_message_metadata_and_status_with_protocol_event(
                session_id,
                &admission_id,
                assistant.id,
                &metadata,
                SessionStatus::Completed,
                &completed,
                turn_id,
                Some(2),
                Some(1),
                None,
            )
            .await
            .expect("complete after steer drain"),
            AdmittedTerminalCommit::Applied
        );
    }

    #[test]
    fn concurrent_steer_acceptance_and_completion_have_one_linearized_winner() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (store, session_id, admission_id, turn_id, assistant) =
            runtime.block_on(active_turn_fixture());
        let paths = store.paths().clone();
        drop(store);
        let barrier = Arc::new(Barrier::new(3));

        let completion_paths = paths.clone();
        let completion_barrier = Arc::clone(&barrier);
        let completion_admission_id = admission_id.clone();
        let completion = std::thread::spawn(move || {
            let store = SqliteStore::open(&completion_paths).expect("completion store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("completion runtime");
            let event = RunEvent::SessionCompleted {
                session_id,
                finish_reason: Some(FinishReason::Stop),
            };
            let metadata = MessageMetadata::Assistant(AssistantMessageMeta {
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                finish_reason: Some(FinishReason::Stop),
                token_usage: None,
                summary: false,
            });
            completion_barrier.wait();
            runtime.block_on(
                store
                    .session_repo()
                    .update_admitted_message_metadata_and_status_with_protocol_event(
                        session_id,
                        &completion_admission_id,
                        assistant.id,
                        &metadata,
                        SessionStatus::Completed,
                        &event,
                        turn_id,
                        Some(1),
                        Some(0),
                        None,
                    ),
            )
        });

        let steer_paths = paths.clone();
        let steer_barrier = Arc::clone(&barrier);
        let steer = std::thread::spawn(move || {
            let store = SqliteStore::open(&steer_paths).expect("steer store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("steer runtime");
            let steer = SteerTurn {
                expected_turn_id: turn_id,
                items: vec![UserInputItem::Text {
                    text: "race the final answer".to_string(),
                }],
                additional_context: Default::default(),
                client_user_message_id: Some("barrier-race".to_string()),
            };
            steer_barrier.wait();
            runtime.block_on(
                store
                    .session_repo()
                    .accept_active_turn_steer(session_id, &steer),
            )
        });

        barrier.wait();
        let completion_applied = completion
            .join()
            .expect("completion thread")
            .expect("completion result");
        let steer_result = steer.join().expect("steer thread");
        let final_store = SqliteStore::open(&paths).expect("final store");
        let final_repo = final_store.session_repo();
        let final_session = runtime
            .block_on(final_repo.get_session(session_id))
            .expect("final session");
        let history = final_store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("final history");
        let accepted_steer_count = history
            .iter()
            .filter(|item| matches!(&item.payload, HistoryItemPayload::SteerTurn { .. }))
            .count();

        match (completion_applied, steer_result) {
            (AdmittedTerminalCommit::Applied, Err(_)) => {
                assert_eq!(final_session.status, SessionStatus::Completed);
                assert_eq!(accepted_steer_count, 0);
            }
            (AdmittedTerminalCommit::UnseenSteer { .. }, Ok(_)) => {
                assert_eq!(final_session.status, SessionStatus::Running);
                assert_eq!(accepted_steer_count, 1);
            }
            (completion_applied, steer_result) => panic!(
                "completion and steer were not linearized: completion={completion_applied:?}, steer={steer_result:?}"
            ),
        }
    }

    #[tokio::test]
    async fn admission_does_not_publish_an_old_turn_as_steer_target() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let old_turn_id = TurnId::new();
        store
            .protocol_event_store()
            .append_history_item(&HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                turn_id: old_turn_id,
                sequence_no: 0,
                created_at_ms: SystemClock.now_ms(),
                payload: HistoryItemPayload::Message {
                    message_id: None,
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: "old turn".to_string(),
                    }],
                },
            })
            .expect("old turn history");
        let admission_id = repo
            .admit_session_run(session_id)
            .await
            .expect("admit")
            .expect("admitted");
        let old_turn_steer = SteerTurn {
            expected_turn_id: old_turn_id,
            items: vec![UserInputItem::Text {
                text: "must not attach to old turn".to_string(),
            }],
            additional_context: Default::default(),
            client_user_message_id: None,
        };
        assert!(
            repo.accept_active_turn_steer(session_id, &old_turn_steer)
                .await
                .is_err()
        );

        let new_turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &admission_id, new_turn_id)
                .await
                .expect("activate new turn")
        );
        assert!(
            repo.accept_active_turn_steer(session_id, &old_turn_steer)
                .await
                .is_err()
        );
        let new_turn_steer = SteerTurn {
            expected_turn_id: new_turn_id,
            ..old_turn_steer
        };
        repo.accept_active_turn_steer(session_id, &new_turn_steer)
            .await
            .expect("new turn steer");
    }

    #[tokio::test]
    async fn awaiting_user_retains_owner_and_blocks_replacement_admission() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let admission_id = repo
            .admit_session_run(session_id)
            .await
            .expect("admit")
            .expect("admitted");
        let turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );
        assert!(
            repo.terminalize_admitted_session_with_protocol_event(
                session_id,
                &admission_id,
                SessionStatus::AwaitingUser,
                &RunEvent::SessionAwaitingUser {
                    session_id,
                    finish_reason: Some(FinishReason::ToolCall),
                },
                turn_id,
                None,
            )
            .await
            .expect("awaiting user")
        );
        assert!(
            repo.admit_session_run(session_id)
                .await
                .expect("replacement admission")
                .is_none()
        );
        assert_eq!(
            repo.admitted_run_status(session_id, &admission_id, turn_id)
                .await
                .expect("owned status"),
            Some(SessionStatus::AwaitingUser)
        );
    }

    #[tokio::test]
    async fn admitted_terminalization_rejects_a_different_turn() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let admission_id = repo
            .admit_session_run(session_id)
            .await
            .expect("admit")
            .expect("admitted");
        let turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );
        assert!(
            !repo
                .terminalize_admitted_session_with_protocol_event(
                    session_id,
                    &admission_id,
                    SessionStatus::Failed,
                    &RunEvent::SessionFailed {
                        session_id,
                        message: "wrong turn".to_string(),
                    },
                    TurnId::new(),
                    None,
                )
                .await
                .expect("wrong turn rejected")
        );
        assert_eq!(
            repo.get_session(session_id).await.expect("session").status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn terminal_commit_fails_unfinished_tools_before_releasing_owner() {
        let (store, session_id, admission_id, turn_id, assistant) = active_turn_fixture().await;
        let repo = store.session_repo();
        let tool_call = repo
            .insert_tool_call(
                session_id,
                assistant.id,
                "shell",
                "{}",
                Some("pending"),
                serde_json::Value::Null,
            )
            .await
            .expect("pending tool");
        let event = RunEvent::SessionFailed {
            session_id,
            message: "agent failure".to_string(),
        };
        assert_eq!(
            repo.update_admitted_message_metadata_and_status_with_protocol_event(
                session_id,
                &admission_id,
                assistant.id,
                &test_assistant_metadata(Some(FinishReason::Error)),
                SessionStatus::Failed,
                &event,
                turn_id,
                None,
                None,
                None,
            )
            .await
            .expect("terminal commit"),
            AdmittedTerminalCommit::Applied
        );
        let tool_status = repo
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT status FROM tool_calls WHERE id = ?1",
                params![tool_call.id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .expect("tool status");
        assert_eq!(tool_status, "failed");
    }

    #[tokio::test]
    async fn externally_stopped_admission_cannot_block_the_active_goal() {
        let (store, session_id, admission_id, turn_id, assistant) = active_turn_fixture().await;
        let repo = store.session_repo();
        repo.replace_thread_goal(
            session_id,
            "keep goal active after external stop",
            ThreadGoalStatus::Active,
            None,
        )
        .await
        .expect("goal");
        let (_, goal_id) = repo
            .get_thread_goal_with_id(session_id)
            .await
            .expect("goal read")
            .expect("stored goal");
        assert!(
            repo.terminalize_active_session_with_protocol_event(
                session_id,
                SessionStatus::Cancelled,
                &RunEvent::SessionInterrupted {
                    session_id,
                    reason: "external stop".to_string(),
                },
                turn_id,
                None,
            )
            .await
            .expect("external stop")
        );

        assert_eq!(
            repo.update_admitted_message_metadata_and_status_with_protocol_event(
                session_id,
                &admission_id,
                assistant.id,
                &test_assistant_metadata(Some(FinishReason::Error)),
                SessionStatus::Failed,
                &RunEvent::SessionFailed {
                    session_id,
                    message: "late agent failure".to_string(),
                },
                turn_id,
                None,
                None,
                Some(&goal_id),
            )
            .await
            .expect("late failure acknowledgement"),
            AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission
        );
        assert_eq!(
            repo.get_thread_goal(session_id)
                .await
                .expect("goal after stop")
                .expect("goal remains")
                .status,
            ThreadGoalStatus::Active
        );
    }

    #[tokio::test]
    async fn stopped_owner_cannot_drain_a_later_turn_steer() {
        let (store, session_id, first_admission, first_turn_id, _) = active_turn_fixture().await;
        let repo = store.session_repo();
        assert!(
            repo.terminalize_active_session_with_protocol_event(
                session_id,
                SessionStatus::Cancelled,
                &RunEvent::SessionInterrupted {
                    session_id,
                    reason: "stop first".to_string(),
                },
                first_turn_id,
                None,
            )
            .await
            .expect("interrupt first")
        );
        assert!(
            repo.list_admitted_turn_steers(session_id, &first_admission, first_turn_id)
                .await
                .expect("old mailbox")
                .is_none()
        );
        assert!(
            store
                .protocol_event_store()
                .append_admitted_event_bundle_allocating(
                    &first_admission,
                    &RuntimeEvent {
                        id: RuntimeEventId::new(),
                        session_id,
                        turn_id: first_turn_id,
                        sequence_no: 0,
                        created_at_ms: SystemClock.now_ms(),
                        msg: RuntimeEventMsg::Warning {
                            message: "late old-owner event".to_string(),
                        },
                    },
                    None,
                    None,
                )
                .expect("late event guard")
                .is_none()
        );
        assert!(
            repo.admit_session_run(session_id)
                .await
                .expect("blocked admission")
                .is_none()
        );
        assert!(
            repo.release_stopped_run_admission(session_id, &first_admission)
                .await
                .expect("release stopped owner")
        );
        let second_admission = repo
            .admit_session_run(session_id)
            .await
            .expect("second admission")
            .expect("second admitted");
        let second_turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &second_admission, second_turn_id)
                .await
                .expect("activate second turn")
        );
        repo.accept_active_turn_steer(
            session_id,
            &SteerTurn {
                expected_turn_id: second_turn_id,
                items: vec![UserInputItem::Text {
                    text: "second steer".to_string(),
                }],
                additional_context: Default::default(),
                client_user_message_id: None,
            },
        )
        .await
        .expect("second steer");
        assert!(
            repo.list_admitted_turn_steers(session_id, &first_admission, first_turn_id)
                .await
                .expect("old mailbox after readmit")
                .is_none()
        );
        assert_eq!(
            repo.list_admitted_turn_steers(session_id, &second_admission, second_turn_id)
                .await
                .expect("new mailbox")
                .expect("new owner")
                .len(),
            1
        );
    }

    #[test]
    fn concurrent_interrupt_never_allows_readmission_before_owner_acknowledgement() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (store, session_id, admission_id, turn_id, _) = runtime.block_on(active_turn_fixture());
        let paths = store.paths().clone();
        drop(store);
        let barrier = Arc::new(Barrier::new(3));

        let interrupt_paths = paths.clone();
        let interrupt_barrier = Arc::clone(&barrier);
        let interrupt = std::thread::spawn(move || {
            let store = SqliteStore::open(&interrupt_paths).expect("interrupt store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("interrupt runtime");
            interrupt_barrier.wait();
            runtime.block_on(
                store
                    .session_repo()
                    .terminalize_active_session_with_protocol_event(
                        session_id,
                        SessionStatus::Cancelled,
                        &RunEvent::SessionInterrupted {
                            session_id,
                            reason: "cross-process stop".to_string(),
                        },
                        turn_id,
                        None,
                    ),
            )
        });
        let admission_paths = paths.clone();
        let admission_barrier = Arc::clone(&barrier);
        let replacement = std::thread::spawn(move || {
            let store = SqliteStore::open(&admission_paths).expect("admission store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("admission runtime");
            admission_barrier.wait();
            runtime.block_on(store.session_repo().admit_session_run(session_id))
        });
        barrier.wait();
        assert!(
            interrupt
                .join()
                .expect("interrupt worker")
                .expect("interrupt result")
        );
        assert!(
            replacement
                .join()
                .expect("admission worker")
                .expect("admission result")
                .is_none()
        );

        let final_store = SqliteStore::open(&paths).expect("final store");
        let final_repo = final_store.session_repo();
        assert!(
            runtime
                .block_on(final_repo.release_stopped_run_admission(session_id, &admission_id))
                .expect("owner acknowledgement")
        );
        assert!(
            runtime
                .block_on(final_repo.admit_session_run(session_id))
                .expect("post-ack admission")
                .is_some()
        );
    }

    #[test]
    fn concurrent_steer_and_runtime_event_share_the_database_allocator() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (store, session_id, _, turn_id, _) = runtime.block_on(active_turn_fixture());
        let paths = store.paths().clone();
        drop(store);
        let warning = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: SystemClock.now_ms(),
            msg: RuntimeEventMsg::Warning {
                message: "race with steer".to_string(),
            },
        };
        let warning_id = warning.id;
        let barrier = Arc::new(Barrier::new(3));

        let event_paths = paths.clone();
        let event_barrier = Arc::clone(&barrier);
        let event_worker = std::thread::spawn(move || {
            let store = SqliteStore::open(&event_paths).expect("event store");
            event_barrier.wait();
            store.protocol_event_store().append_runtime_event(&warning)
        });
        let steer_paths = paths.clone();
        let steer_barrier = Arc::clone(&barrier);
        let steer_worker = std::thread::spawn(move || {
            let store = SqliteStore::open(&steer_paths).expect("steer store");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("steer runtime");
            steer_barrier.wait();
            runtime.block_on(store.session_repo().accept_active_turn_steer(
                session_id,
                &SteerTurn {
                    expected_turn_id: turn_id,
                    items: vec![UserInputItem::Text {
                        text: "allocator race".to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("allocator-race".to_string()),
                },
            ))
        });
        barrier.wait();
        event_worker
            .join()
            .expect("event worker")
            .expect("event append");
        steer_worker
            .join()
            .expect("steer worker")
            .expect("steer append");

        let final_store = SqliteStore::open(&paths).expect("final store");
        let events = final_store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("events");
        let warning_sequence = events
            .iter()
            .find(|event| event.id == warning_id)
            .expect("warning id preserved")
            .sequence_no;
        let steer_sequence = events
            .iter()
            .find(|event| matches!(event.msg, RuntimeEventMsg::SteerInputAccepted { .. }))
            .expect("steer event")
            .sequence_no;
        assert_ne!(warning_sequence, steer_sequence);
    }

    #[tokio::test]
    async fn stale_admission_cannot_terminalize_a_newer_run() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let first_admission = repo
            .admit_session_run(session_id)
            .await
            .expect("first admission")
            .expect("first admitted");
        let first_turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &first_admission, first_turn_id)
                .await
                .expect("activate first turn")
        );
        assert!(
            repo.terminalize_active_session_with_protocol_event(
                session_id,
                SessionStatus::Cancelled,
                &RunEvent::SessionInterrupted {
                    session_id,
                    reason: "replace run".to_string(),
                },
                first_turn_id,
                None,
            )
            .await
            .expect("cancel first")
        );
        assert!(
            repo.admit_session_run(session_id)
                .await
                .expect("blocked second admission")
                .is_none(),
            "an interrupted run retains its admission until the matching owner acknowledges stop"
        );

        assert!(
            repo.terminalize_admitted_session_with_protocol_event(
                session_id,
                &first_admission,
                SessionStatus::Cancelled,
                &RunEvent::SessionInterrupted {
                    session_id,
                    reason: "owner acknowledged stop".to_string(),
                },
                first_turn_id,
                None,
            )
            .await
            .expect("first owner acknowledgement")
        );
        let second_admission = repo
            .admit_session_run(session_id)
            .await
            .expect("second admission")
            .expect("second admitted after acknowledgement");
        let second_turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &second_admission, second_turn_id)
                .await
                .expect("activate second turn")
        );
        assert!(
            !repo
                .terminalize_admitted_session_with_protocol_event(
                    session_id,
                    &first_admission,
                    SessionStatus::Failed,
                    &RunEvent::SessionFailed {
                        session_id,
                        message: "stale cleanup".to_string(),
                    },
                    first_turn_id,
                    None,
                )
                .await
                .expect("stale cleanup rejected")
        );
        assert_eq!(
            repo.get_session(session_id).await.expect("new run").status,
            SessionStatus::Running
        );
        assert!(
            repo.terminalize_admitted_session_with_protocol_event(
                session_id,
                &second_admission,
                SessionStatus::Failed,
                &RunEvent::SessionFailed {
                    session_id,
                    message: "current cleanup".to_string(),
                },
                second_turn_id,
                None,
            )
            .await
            .expect("current cleanup")
        );
    }

    #[tokio::test]
    async fn completed_commit_cannot_overwrite_an_interrupted_session() {
        let (store, session_id) = test_repo().await;
        let repo = store.session_repo();
        let admission_id = repo
            .admit_session_run(session_id)
            .await
            .expect("admit")
            .expect("admitted");
        let turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );
        let (assistant, _) = repo
            .append_assistant_message_with_protocol_start(
                NewMessage {
                    session_id,
                    parent_message_id: None,
                    role: MessageRole::Assistant,
                    metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                        model: "model".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                &admission_id,
                turn_id,
                None,
                "model".to_string(),
            )
            .await
            .expect("assistant");
        let interrupted = RunEvent::SessionInterrupted {
            session_id,
            reason: "stop".to_string(),
        };
        assert!(
            repo.terminalize_active_session_with_protocol_event(
                session_id,
                SessionStatus::Cancelled,
                &interrupted,
                turn_id,
                None,
            )
            .await
            .expect("interrupt")
        );
        let completed = RunEvent::SessionCompleted {
            session_id,
            finish_reason: Some(FinishReason::Stop),
        };
        let completion_applied = repo
            .update_message_metadata_and_status_with_protocol_event(
                session_id,
                assistant.id,
                &MessageMetadata::Assistant(AssistantMessageMeta {
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                    finish_reason: Some(FinishReason::Stop),
                    token_usage: None,
                    summary: false,
                }),
                SessionStatus::Completed,
                &completed,
                turn_id,
                None,
            )
            .await
            .expect("complete attempt");

        assert!(!completion_applied);
        assert_eq!(
            repo.get_session(session_id).await.expect("session").status,
            SessionStatus::Cancelled
        );
    }
}

fn session_status_text(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Completed => "completed",
        SessionStatus::AwaitingUser => "awaiting_user",
        SessionStatus::Cancelled => "cancelled",
        SessionStatus::Failed => "failed",
    }
}

fn terminal_status_text(status: SessionStatus) -> Result<&'static str, StorageError> {
    match status {
        SessionStatus::Completed => Ok("completed"),
        SessionStatus::AwaitingUser => Ok("awaiting_user"),
        SessionStatus::Cancelled => Ok("cancelled"),
        SessionStatus::Failed => Ok("failed"),
        SessionStatus::Idle | SessionStatus::Running => Err(StorageError::Message(
            "active session terminalization requires a terminal status".to_string(),
        )),
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
    let was_active = matches!(current_status, "running" | "awaiting_user");
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
             SET status = 'cancelled',
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
    fail_unfinished_tool_calls_in_connection(
        transaction,
        session_id,
        EXPIRED_RUN_RECOVERY_REASON,
        now_ms,
    )?;
    if was_active && let Some(turn_id) = recovery_turn_id {
        insert_protocol_projection_if_requested(
            transaction,
            &RunEvent::SessionInterrupted {
                session_id,
                reason: EXPIRED_RUN_RECOVERY_REASON.to_string(),
            },
            Some(session_id),
            turn_id,
            Some(0),
        )?;
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
               AND status IN ('running', 'awaiting_user')",
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

fn fail_unfinished_tool_calls_in_connection(
    connection: &Connection,
    session_id: SessionId,
    error_text: &str,
    finished_at_ms: i64,
) -> Result<(), StorageError> {
    connection.execute(
        "UPDATE tool_calls
         SET status = 'failed',
             error_text = COALESCE(error_text, ?2),
             finished_at_ms = COALESCE(finished_at_ms, ?3)
         WHERE session_id = ?1 AND status IN ('pending', 'running')",
        params![session_id.to_string(), error_text, finished_at_ms],
    )?;
    Ok(())
}

fn unfinished_tool_reason_for_event(event: &RunEvent) -> &str {
    match event {
        RunEvent::SessionInterrupted { reason, .. } => reason,
        RunEvent::SessionFailed { message, .. } => message,
        RunEvent::SessionCompleted { .. } => "run completed before a tool call finished",
        RunEvent::SessionAwaitingUser { .. } => "run paused before a tool call finished",
        _ => "run terminalized before a tool call finished",
    }
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
