use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camino::Utf8Path;
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::AccessMode;
use crate::error::StorageError;
use crate::protocol::{
    ProtocolEventStore, ToolProgressEffect, TurnId, UserTurn, VerificationRunResult,
    VerificationRunStatus, project_protocol_run_event,
};
use crate::runtime::{Clock, SystemClock};
use crate::session::{
    CompletionState, DiffSummaryPart, FailureKind, FailureState, MessageId, MessageMetadata,
    MessagePart, MessageRecord, MessageRole, NewMessage, NewPart, NewSession, PartKind, PartRecord,
    ProcessPhase, ProjectId, ProjectRepository, RunEvent, SessionId, SessionMemoryMode,
    SessionMemoryModeUpdate, SessionModelParameters, SessionRecord, SessionRepository,
    SessionSettingsPatch, SessionSettingsUpdate, SessionStateSnapshot, SessionStatus,
    SessionTitleUpdate, TaskRoute, TextPart, TodoItem, TodoKind, TodoPriority, TodoStatus,
    ToolCallId, ToolCallPart, ToolCallRecord, ToolCallStatus, ToolResultPart, Transcript,
    TranscriptMessage, VerificationState,
};

const STORAGE_REPOSITORY_FIXTURE_MODEL: &str = crate::storage::STORAGE_REPOSITORY_FIXTURE_MODEL;
const STORAGE_REPOSITORY_FIXTURE_BASE_URL: &str =
    crate::storage::STORAGE_REPOSITORY_FIXTURE_BASE_URL;

#[derive(Clone)]
pub struct SqliteSessionRepository {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteSessionRepository {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
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
        let now = SystemClock.now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![session_id.to_string()],
        )?;
        upsert_session_state_row(&transaction, session_id, state, now)?;
        transaction.execute(
            "UPDATE sessions
             SET status = 'idle', updated_at_ms = ?2, completed_at_ms = NULL
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
        let now = SystemClock.now_ms();
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

    pub async fn append_user_message_with_protocol_bundle(
        &self,
        draft: NewMessage,
        parts: Vec<NewPart>,
        initial_state: &SessionStateSnapshot,
        turn: &UserTurn,
        protocol_turn_id: TurnId,
        protocol_sequence_no: i64,
    ) -> Result<MessageRecord, StorageError> {
        let id = MessageId::new();
        let now = SystemClock.now_ms();
        let metadata_json = serde_json::to_string(&draft.metadata)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
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
        transaction.execute(
            "UPDATE sessions SET status = 'running', updated_at_ms = ?2, completed_at_ms = NULL WHERE id = ?1",
            params![draft.session_id.to_string(), now],
        )?;

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
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        model: String,
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
        message_id: MessageId,
        part: NewPart,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
    ) -> Result<PartRecord, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
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
            "UPDATE sessions SET status = ?2, updated_at_ms = ?3, completed_at_ms = ?4 WHERE id = ?1",
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

    pub async fn record_pending_tool_call_with_protocol_bundle(
        &self,
        session_id: SessionId,
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
        let transaction = connection.transaction()?;
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
        let transaction = connection.transaction()?;
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
        let transaction = connection.transaction()?;
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
        let transaction = connection.transaction()?;
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
            "UPDATE messages SET metadata_json = ?2 WHERE id = ?1",
            params![message_id.to_string(), serde_json::to_string(metadata)?],
        )?;
        transaction.execute(
            "UPDATE sessions SET status = ?2, updated_at_ms = ?3, completed_at_ms = ?4 WHERE id = ?1",
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
        let normalized = format!("%{}%", query.trim().to_ascii_lowercase());
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
                   lower(title) LIKE ?2
                   OR lower(cwd_path) LIKE ?2
                   OR lower(model_name) LIKE ?2
                   OR lower(base_url) LIKE ?2
                   OR lower(access_mode) LIKE ?2
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
        connection.execute(
            "UPDATE sessions SET archived_at_ms = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), archived_at_ms, now],
        )?;
        drop(connection);
        self.get_session(id).await
    }

    async fn update_session_settings(
        &self,
        id: SessionId,
        patch: &SessionSettingsPatch,
    ) -> Result<SessionSettingsUpdate, StorageError> {
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
        let now = SystemClock::now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE sessions
             SET cwd_path = ?2, model_name = ?3, base_url = ?4, access_mode = ?5, model_parameters_json = ?6, updated_at_ms = ?7
             WHERE id = ?1",
            params![
                id.to_string(),
                next_cwd.as_str(),
                next_model,
                next_base_url,
                next_access_mode.as_str(),
                serde_json::to_string(&next_model_parameters)?,
                now
            ],
        )?;
        drop(connection);
        Ok(SessionSettingsUpdate {
            session: self.get_session(id).await?,
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
    )
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
        "skill" => crate::tool::ToolName::Skill,
        "docling_convert" => crate::tool::ToolName::DoclingConvert,
        "mcp_call" => crate::tool::ToolName::McpCall,
        "todowrite" => crate::tool::ToolName::TodoWrite,
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
        crate::tool::ToolName::Skill => "skill",
        crate::tool::ToolName::DoclingConvert => "docling_convert",
        crate::tool::ToolName::McpCall => "mcp_call",
        crate::tool::ToolName::TodoWrite => "todowrite",
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

pub(crate) fn todo_update_uses_single_unit_of_work_fixture_passes() -> bool {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::Builder::new()
            .name("moyai-todo-update-unit-fixture".to_string())
            .spawn(todo_update_uses_single_unit_of_work_fixture_inner)
            .ok()
            .and_then(|handle| handle.join().ok())
            .unwrap_or(false);
    }
    todo_update_uses_single_unit_of_work_fixture_inner()
}

fn todo_update_uses_single_unit_of_work_fixture_inner() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(data_dir) = Utf8Path::from_path(temp.path()) else {
        return false;
    };
    let paths = crate::storage::StoragePaths {
        data_dir: data_dir.to_path_buf(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let store = match crate::storage::SqliteStore::open(&paths) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if store.migrate().is_err() {
        return false;
    }
    let project_repo = store.project_repo();
    let session_repo = store.session_repo();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(value) => value,
        Err(_) => return false,
    };

    runtime.block_on(async {
        let project_id = crate::session::ProjectId::new();
        let root = Utf8Path::new("C:/workspace/todo-unit");
        if project_repo
            .upsert_project(project_id, root, "Todo Unit", "none")
            .await
            .is_err()
        {
            return false;
        }
        let session = match session_repo
            .create_session(NewSession {
                project_id,
                title: "todo unit".to_string(),
                cwd: root.to_path_buf(),
                model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };

        let original = todo_fixture_item(crate::session::TodoId::new(), "original");
        if session_repo
            .update_todos(session.id, &[original.clone()])
            .await
            .is_err()
        {
            return false;
        }
        let duplicate_id = crate::session::TodoId::new();
        let duplicate_a = todo_fixture_item(duplicate_id, "replacement-a");
        let duplicate_b = todo_fixture_item(duplicate_id, "replacement-b");
        if session_repo
            .update_todos(session.id, &[duplicate_a, duplicate_b])
            .await
            .is_ok()
        {
            return false;
        }
        let todos = match session_repo.list_todos(session.id).await {
            Ok(value) => value,
            Err(_) => return false,
        };
        todos.len() == 1 && todos[0].id == original.id && todos[0].content == original.content
    })
}

fn todo_fixture_item(id: crate::session::TodoId, content: &str) -> TodoItem {
    TodoItem {
        id,
        content: content.to_string(),
        kind: TodoKind::Work,
        status: TodoStatus::Pending,
        priority: TodoPriority::Medium,
        targets: Vec::new(),
        depends_on: Vec::new(),
        success_criteria: Vec::new(),
        blocked_by: Vec::new(),
    }
}

pub(crate) fn protocol_message_parts_use_single_unit_of_work_fixture_passes() -> bool {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::Builder::new()
            .name("moyai-protocol-message-unit-fixture".to_string())
            .spawn(protocol_message_parts_use_single_unit_of_work_fixture_inner)
            .ok()
            .and_then(|handle| handle.join().ok())
            .unwrap_or(false);
    }
    protocol_message_parts_use_single_unit_of_work_fixture_inner()
}

fn protocol_message_parts_use_single_unit_of_work_fixture_inner() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(data_dir) = Utf8Path::from_path(temp.path()) else {
        return false;
    };
    let paths = crate::storage::StoragePaths {
        data_dir: data_dir.to_path_buf(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let store = match crate::storage::SqliteStore::open(&paths) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if store.migrate().is_err() {
        return false;
    }
    let project_repo = store.project_repo();
    let session_repo = store.session_repo();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(value) => value,
        Err(_) => return false,
    };

    runtime.block_on(async {
        let project_id = crate::session::ProjectId::new();
        let root = Utf8Path::new("C:/workspace/persistence-unit");
        if project_repo
            .upsert_project(project_id, root, "Persistence Unit", "none")
            .await
            .is_err()
        {
            return false;
        }
        let session = match session_repo
            .create_session(NewSession {
                project_id,
                title: "message parts unit".to_string(),
                cwd: root.to_path_buf(),
                model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let turn_id = TurnId::new();
        let message = match session_repo
            .append_message_with_parts_and_protocol_event(
                NewMessage {
                    session_id: session.id,
                    parent_message_id: None,
                    role: MessageRole::User,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: root.to_path_buf(),
                        requested_model: None,
                        editor_context: None,
                    }),
                },
                vec![
                    NewPart {
                        kind: PartKind::Text,
                        payload: MessagePart::Text(TextPart {
                            text: "first".to_string(),
                        }),
                    },
                    NewPart {
                        kind: PartKind::Text,
                        payload: MessagePart::Text(TextPart {
                            text: "second".to_string(),
                        }),
                    },
                ],
                |message_id| RunEvent::UserMessageStored { message_id },
                turn_id,
                Some(0),
            )
            .await
        {
            Ok((value, _event)) => value,
            Err(_) => return false,
        };
        let transcript = match session_repo.compatibility_transcript(session.id).await {
            Ok(value) => value,
            Err(_) => return false,
        };
        let Some(stored) = transcript
            .messages
            .iter()
            .find(|entry| entry.record.id == message.id)
        else {
            return false;
        };
        stored.parts.len() == 2
            && stored.parts[0].sequence_no == 1
            && stored.parts[1].sequence_no == 2
    })
}

pub(crate) fn tool_output_filechange_projection_single_unit_of_work_fixture_passes() -> bool {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::Builder::new()
            .name("moyai-tool-output-filechange-unit-fixture".to_string())
            .spawn(tool_output_filechange_projection_single_unit_of_work_fixture_inner)
            .ok()
            .and_then(|handle| handle.join().ok())
            .unwrap_or(false);
    }
    tool_output_filechange_projection_single_unit_of_work_fixture_inner()
}

fn tool_output_filechange_projection_single_unit_of_work_fixture_inner() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(data_dir) = Utf8Path::from_path(temp.path()) else {
        return false;
    };
    let paths = crate::storage::StoragePaths {
        data_dir: data_dir.to_path_buf(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let store = match crate::storage::SqliteStore::open(&paths) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if store.migrate().is_err() {
        return false;
    }
    let project_repo = store.project_repo();
    let session_repo = store.session_repo();
    let protocol_store = store.protocol_event_store();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(value) => value,
        Err(_) => return false,
    };

    runtime.block_on(async {
        let project_id = crate::session::ProjectId::new();
        let root = Utf8Path::new("C:/workspace/tool-output-filechange-unit");
        if project_repo
            .upsert_project(project_id, root, "Tool Output FileChange Unit", "none")
            .await
            .is_err()
        {
            return false;
        }
        let session = match session_repo
            .create_session(NewSession {
                project_id,
                title: "tool output filechange unit".to_string(),
                cwd: root.to_path_buf(),
                model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let turn_id = TurnId::new();
        let (assistant, _start_event) = match session_repo
            .append_assistant_message_with_protocol_start(
                NewMessage {
                    session_id: session.id,
                    parent_message_id: None,
                    role: MessageRole::Assistant,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                        base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                turn_id,
                Some(0),
                STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
            )
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let (tool_call, _pending_event) = match session_repo
            .record_pending_tool_call_with_protocol_bundle(
                session.id,
                assistant.id,
                "write",
                r#"{"path":"src/lib.rs","content":"ok"}"#,
                Some("write src/lib.rs"),
                serde_json::json!({"tool_route":{"original_arguments_json":"{}"}}),
                turn_id,
                Some(1),
            )
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let change_id = crate::session::ChangeId::new();
        let change = crate::edit::ChangeSummary {
            change_id,
            kind: crate::session::ChangeKind::Update,
            path_before: Some(camino::Utf8PathBuf::from("src/lib.rs")),
            path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
        };
        let diff_summary = DiffSummaryPart {
            tool_call_id: Some(tool_call.id),
            change_ids: vec![change_id],
            changes: vec![crate::protocol::FileChangeEvidence {
                change_id,
                kind: crate::session::ChangeKind::Update,
                path_before: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                summary: "Updated src/lib.rs".to_string(),
            }],
            summary: "Updated src/lib.rs".to_string(),
        };
        let duplicate_sequence_result = session_repo
            .complete_tool_call_with_file_changes_protocol_bundle(
                session.id,
                assistant.id,
                tool_call.id,
                crate::tool::ToolName::Write,
                "write src/lib.rs",
                serde_json::json!({"success": true, "progress_effect": "made_progress"}),
                "Updated src/lib.rs",
                None,
                diff_summary.clone(),
                vec![change.clone()],
                turn_id,
                Some(2),
                Some(2),
            )
            .await;
        if duplicate_sequence_result.is_ok() {
            return false;
        }
        let status_after_failed_bundle = {
            let connection = session_repo
                .connection
                .lock()
                .expect("sqlite mutex poisoned");
            match connection.query_row(
                "SELECT status FROM tool_calls WHERE id = ?1",
                params![tool_call.id.to_string()],
                |row| row.get::<_, String>(0),
            ) {
                Ok(value) => value,
                Err(_) => return false,
            }
        };
        if status_after_failed_bundle != "pending" {
            return false;
        }
        let transcript_after_failed_bundle =
            match session_repo.compatibility_transcript(session.id).await {
                Ok(value) => value,
                Err(_) => return false,
            };
        let Some(failed_bundle_message) = transcript_after_failed_bundle
            .messages
            .iter()
            .find(|entry| entry.record.id == assistant.id)
        else {
            return false;
        };
        if failed_bundle_message
            .parts
            .iter()
            .any(|part| matches!(part.kind, PartKind::ToolResult | PartKind::DiffSummary))
        {
            return false;
        }
        let runtime_events_after_failed_bundle =
            match protocol_store.list_runtime_events(session.id, turn_id) {
                Ok(value) => value,
                Err(_) => return false,
            };
        if runtime_events_after_failed_bundle
            .iter()
            .any(|event| event.sequence_no == 2)
        {
            return false;
        }
        if session_repo
            .complete_tool_call_with_file_changes_protocol_bundle(
                session.id,
                assistant.id,
                tool_call.id,
                crate::tool::ToolName::Write,
                "write src/lib.rs",
                serde_json::json!({"success": true, "progress_effect": "made_progress"}),
                "Updated src/lib.rs",
                None,
                diff_summary,
                vec![change],
                turn_id,
                Some(2),
                Some(3),
            )
            .await
            .is_err()
        {
            return false;
        }
        let transcript_after_success = match session_repo.compatibility_transcript(session.id).await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let Some(success_message) = transcript_after_success
            .messages
            .iter()
            .find(|entry| entry.record.id == assistant.id)
        else {
            return false;
        };
        let has_tool_result = success_message
            .parts
            .iter()
            .any(|part| matches!(part.kind, PartKind::ToolResult));
        let has_diff_summary = success_message
            .parts
            .iter()
            .any(|part| matches!(part.kind, PartKind::DiffSummary));
        let runtime_events_after_success =
            match protocol_store.list_runtime_events(session.id, turn_id) {
                Ok(value) => value,
                Err(_) => return false,
            };
        has_tool_result
            && has_diff_summary
            && runtime_events_after_success
                .iter()
                .any(|event| event.sequence_no == 2)
            && runtime_events_after_success
                .iter()
                .any(|event| event.sequence_no == 3)
    })
}

pub(crate) fn tool_output_filechange_projection_owner_coherence_fixture_passes() -> bool {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::Builder::new()
            .name("moyai-tool-output-filechange-owner-fixture".to_string())
            .spawn(tool_output_filechange_projection_owner_coherence_fixture_inner)
            .ok()
            .and_then(|handle| handle.join().ok())
            .unwrap_or(false);
    }
    tool_output_filechange_projection_owner_coherence_fixture_inner()
}

fn tool_output_filechange_projection_owner_coherence_fixture_inner() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(data_dir) = Utf8Path::from_path(temp.path()) else {
        return false;
    };
    let paths = crate::storage::StoragePaths {
        data_dir: data_dir.to_path_buf(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let store = match crate::storage::SqliteStore::open(&paths) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if store.migrate().is_err() {
        return false;
    }
    let project_repo = store.project_repo();
    let session_repo = store.session_repo();
    let protocol_store = store.protocol_event_store();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(value) => value,
        Err(_) => return false,
    };

    runtime.block_on(async {
        let project_id = crate::session::ProjectId::new();
        let root = Utf8Path::new("C:/workspace/tool-output-filechange-owner");
        if project_repo
            .upsert_project(project_id, root, "Tool Output FileChange Owner", "none")
            .await
            .is_err()
        {
            return false;
        }
        let session = match session_repo
            .create_session(NewSession {
                project_id,
                title: "tool output filechange owner".to_string(),
                cwd: root.to_path_buf(),
                model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let turn_id = TurnId::new();
        let (assistant, _start_event) = match session_repo
            .append_assistant_message_with_protocol_start(
                NewMessage {
                    session_id: session.id,
                    parent_message_id: None,
                    role: MessageRole::Assistant,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                        base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                turn_id,
                Some(0),
                STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
            )
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let (tool_call, _pending_event) = match session_repo
            .record_pending_tool_call_with_protocol_bundle(
                session.id,
                assistant.id,
                "write",
                r#"{"path":"src/lib.rs","content":"ok"}"#,
                Some("write src/lib.rs"),
                serde_json::json!({"tool_route":{"original_arguments_json":"{}"}}),
                turn_id,
                Some(1),
            )
            .await
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        let change_id = crate::session::ChangeId::new();
        let other_change_id = crate::session::ChangeId::new();
        let change = crate::edit::ChangeSummary {
            change_id,
            kind: crate::session::ChangeKind::Update,
            path_before: Some(camino::Utf8PathBuf::from("src/lib.rs")),
            path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
        };
        let wrong_owner_diff_summary = DiffSummaryPart {
            tool_call_id: Some(crate::session::ToolCallId::new()),
            change_ids: vec![change_id],
            changes: vec![crate::protocol::FileChangeEvidence {
                change_id,
                kind: crate::session::ChangeKind::Update,
                path_before: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                summary: "Updated src/lib.rs".to_string(),
            }],
            summary: "Updated src/lib.rs".to_string(),
        };
        if session_repo
            .complete_tool_call_with_file_changes_protocol_bundle(
                session.id,
                assistant.id,
                tool_call.id,
                crate::tool::ToolName::Write,
                "write src/lib.rs",
                serde_json::json!({"success": true, "progress_effect": "made_progress"}),
                "Updated src/lib.rs",
                None,
                wrong_owner_diff_summary,
                vec![change.clone()],
                turn_id,
                Some(2),
                Some(3),
            )
            .await
            .is_ok()
        {
            return false;
        }
        let mismatched_diff_summary = DiffSummaryPart {
            tool_call_id: Some(tool_call.id),
            change_ids: vec![other_change_id],
            changes: vec![crate::protocol::FileChangeEvidence {
                change_id: other_change_id,
                kind: crate::session::ChangeKind::Update,
                path_before: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                summary: "Updated src/lib.rs".to_string(),
            }],
            summary: "Updated src/lib.rs".to_string(),
        };
        if session_repo
            .complete_tool_call_with_file_changes_protocol_bundle(
                session.id,
                assistant.id,
                tool_call.id,
                crate::tool::ToolName::Write,
                "write src/lib.rs",
                serde_json::json!({"success": true, "progress_effect": "made_progress"}),
                "Updated src/lib.rs",
                None,
                mismatched_diff_summary,
                vec![change.clone()],
                turn_id,
                Some(2),
                Some(3),
            )
            .await
            .is_ok()
        {
            return false;
        }
        let status_after_rejected_bundle = {
            let connection = session_repo
                .connection
                .lock()
                .expect("sqlite mutex poisoned");
            match connection.query_row(
                "SELECT status FROM tool_calls WHERE id = ?1",
                params![tool_call.id.to_string()],
                |row| row.get::<_, String>(0),
            ) {
                Ok(value) => value,
                Err(_) => return false,
            }
        };
        if status_after_rejected_bundle != "pending" {
            return false;
        }
        let transcript_after_rejected_bundle =
            match session_repo.compatibility_transcript(session.id).await {
                Ok(value) => value,
                Err(_) => return false,
            };
        let Some(rejected_bundle_message) = transcript_after_rejected_bundle
            .messages
            .iter()
            .find(|entry| entry.record.id == assistant.id)
        else {
            return false;
        };
        if rejected_bundle_message
            .parts
            .iter()
            .any(|part| matches!(part.kind, PartKind::ToolResult | PartKind::DiffSummary))
        {
            return false;
        }
        let runtime_events_after_rejected_bundle =
            match protocol_store.list_runtime_events(session.id, turn_id) {
                Ok(value) => value,
                Err(_) => return false,
            };
        if runtime_events_after_rejected_bundle
            .iter()
            .any(|event| event.sequence_no == 2 || event.sequence_no == 3)
        {
            return false;
        }
        let valid_diff_summary = DiffSummaryPart {
            tool_call_id: Some(tool_call.id),
            change_ids: vec![change_id],
            changes: vec![crate::protocol::FileChangeEvidence {
                change_id,
                kind: crate::session::ChangeKind::Update,
                path_before: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                summary: "Updated src/lib.rs".to_string(),
            }],
            summary: "Updated src/lib.rs".to_string(),
        };
        let Ok((_tool_output_event, file_changes_event)) = session_repo
            .complete_tool_call_with_file_changes_protocol_bundle(
                session.id,
                assistant.id,
                tool_call.id,
                crate::tool::ToolName::Write,
                "write src/lib.rs",
                serde_json::json!({"success": true, "progress_effect": "made_progress"}),
                "Updated src/lib.rs",
                None,
                valid_diff_summary,
                vec![change.clone()],
                turn_id,
                Some(2),
                Some(3),
            )
            .await
        else {
            return false;
        };
        if !matches!(
            file_changes_event,
            RunEvent::FileChangesRecorded {
                tool_call_id,
                ref changes
            } if tool_call_id == tool_call.id
                && changes.len() == 1
                && changes[0].change_id == change_id
        ) {
            return false;
        }
        let runtime_events_after_success =
            match protocol_store.list_runtime_events(session.id, turn_id) {
                Ok(value) => value,
                Err(_) => return false,
            };
        runtime_events_after_success.iter().any(|event| {
            event.sequence_no == 3
                && matches!(
                    &event.msg,
                    crate::protocol::RuntimeEventMsg::FileChangesRecorded {
                        call_id,
                        change_ids,
                        ..
                    } if *call_id == tool_call.id
                        && change_ids.as_slice() == [change_id]
                )
        })
    })
}

pub(crate) fn session_archive_search_lifecycle_fixture_passes() -> bool {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::Builder::new()
            .name("moyai-session-archive-search-fixture".to_string())
            .spawn(session_archive_search_lifecycle_fixture_inner)
            .ok()
            .and_then(|handle| handle.join().ok())
            .unwrap_or(false);
    }
    session_archive_search_lifecycle_fixture_inner()
}

fn session_archive_search_lifecycle_fixture_inner() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Some(data_dir) = Utf8Path::from_path(temp.path()) else {
        return false;
    };
    let paths = crate::storage::StoragePaths {
        data_dir: data_dir.to_path_buf(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let Ok(store) = crate::storage::SqliteStore::open(&paths) else {
        return false;
    };
    if store.migrate().is_err() {
        return false;
    }
    let project_repo = store.project_repo();
    let session_repo = store.session_repo();
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };

    runtime.block_on(async {
        let project_id = ProjectId::new();
        let root = Utf8Path::new("C:/workspace/session-lifecycle");
        if project_repo
            .upsert_project(project_id, root, "Archive Search Lifecycle", "none")
            .await
            .is_err()
        {
            return false;
        }
        let visible = match session_repo
            .create_session(NewSession {
                project_id,
                title: "visible session".to_string(),
                cwd: root.to_path_buf(),
                model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
        {
            Ok(session) => session,
            Err(_) => return false,
        };
        let archived = match session_repo
            .create_session(NewSession {
                project_id,
                title: "hidden needle target".to_string(),
                cwd: root.to_path_buf(),
                model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
        {
            Ok(session) => session,
            Err(_) => return false,
        };
        if session_repo
            .set_session_archived(archived.id, true)
            .await
            .is_err()
        {
            return false;
        }

        let Ok(default_list) = session_repo.list_sessions(project_id, 10).await else {
            return false;
        };
        if default_list.iter().any(|session| session.id == archived.id)
            || !default_list.iter().any(|session| session.id == visible.id)
        {
            return false;
        }
        let Ok(default_search) = session_repo
            .search_sessions(project_id, "needle", 10, false)
            .await
        else {
            return false;
        };
        if !default_search.is_empty() {
            return false;
        }
        let Ok(inclusive_search) = session_repo
            .search_sessions(project_id, "needle", 10, true)
            .await
        else {
            return false;
        };
        if !inclusive_search
            .iter()
            .any(|session| session.id == archived.id)
        {
            return false;
        }
        let Ok(inclusive_list) = session_repo
            .list_sessions_with_archived(project_id, 10, true)
            .await
        else {
            return false;
        };
        if !inclusive_list
            .iter()
            .any(|session| session.id == archived.id)
            || !inclusive_list
                .iter()
                .any(|session| session.id == visible.id)
        {
            return false;
        }
        if session_repo
            .set_session_archived(archived.id, false)
            .await
            .is_err()
        {
            return false;
        }
        let Ok(unarchived_list) = session_repo.list_sessions(project_id, 10).await else {
            return false;
        };
        unarchived_list
            .iter()
            .any(|session| session.id == archived.id)
    })
}

#[cfg(test)]
pub(crate) fn storage_repository_current_provider_profile_fixture_passes() -> bool {
    STORAGE_REPOSITORY_FIXTURE_MODEL == "qwen/qwen3.6-35b-a3b"
        && STORAGE_REPOSITORY_FIXTURE_BASE_URL == "http://127.0.0.1:1234"
        && !STORAGE_REPOSITORY_FIXTURE_MODEL.contains("local")
        && !STORAGE_REPOSITORY_FIXTURE_BASE_URL.contains("localhost")
        && !STORAGE_REPOSITORY_FIXTURE_BASE_URL.contains("127.0.0.1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ProtocolEventStore, RuntimeEventMsg, TurnId};
    use crate::session::{
        AssistantMessageMeta, FinishReason, ProjectId, ProjectRepository, TokenAccountingSource,
        TokenAccountingState, TokenUsage,
    };
    use crate::storage::{SqliteStore, StoragePaths};
    use camino::Utf8Path;

    #[test]
    fn token_accounting_round_trips_in_session_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
        let paths = StoragePaths {
            data_dir: data_dir.to_path_buf(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("open sqlite");
        store.migrate().expect("migrate sqlite");
        let project_repo = store.project_repo();
        let session_repo = store.session_repo();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let project_id = ProjectId::new();
            let root = Utf8Path::new("C:/workspace/token-accounting");
            project_repo
                .upsert_project(project_id, root, "Token Accounting", "none")
                .await
                .expect("insert project");
            let session = session_repo
                .create_session(NewSession {
                    project_id,
                    title: "state roundtrip".to_string(),
                    cwd: root.to_path_buf(),
                    model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                    base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                    access_mode: crate::config::AccessMode::Default,
                })
                .await
                .expect("insert session");

            let mut state = session_repo
                .get_state(session.id)
                .await
                .expect("load state");
            state.token_accounting = TokenAccountingState::from_provider_usage(
                4096,
                &TokenUsage {
                    prompt_tokens: 123,
                    completion_tokens: 45,
                    total_tokens: 168,
                    reasoning_tokens: Some(9),
                },
            );
            session_repo
                .update_state_with_protocol_event(
                    session.id,
                    &state,
                    &RunEvent::StateUpdated {
                        session_id: session.id,
                        state: state.clone(),
                    },
                    TurnId::new(),
                    Some(0),
                )
                .await
                .expect("persist state");

            let loaded = session_repo
                .get_state(session.id)
                .await
                .expect("reload state");
            assert_eq!(loaded.token_accounting.active_context_tokens, 168);
            assert_eq!(loaded.token_accounting.context_window, Some(4096));
            assert_eq!(
                loaded.token_accounting.last_provider_prompt_tokens,
                Some(123)
            );
            assert_eq!(
                loaded.token_accounting.last_provider_completion_tokens,
                Some(45)
            );
            assert_eq!(
                loaded.token_accounting.last_provider_reasoning_tokens,
                Some(9)
            );
            assert_eq!(
                loaded.token_accounting.source,
                TokenAccountingSource::ProviderReported
            );
        });
    }

    #[test]
    fn protocol_message_parts_use_single_unit_of_work() {
        assert!(super::protocol_message_parts_use_single_unit_of_work_fixture_passes());
    }

    #[test]
    fn tool_output_filechange_projection_uses_single_unit_of_work() {
        assert!(super::tool_output_filechange_projection_single_unit_of_work_fixture_passes());
    }

    #[test]
    fn tool_output_filechange_projection_rejects_mismatched_owner() {
        assert!(super::tool_output_filechange_projection_owner_coherence_fixture_passes());
    }

    #[test]
    fn todo_update_uses_single_unit_of_work() {
        assert!(super::todo_update_uses_single_unit_of_work_fixture_passes());
    }

    #[test]
    fn assistant_terminal_status_and_protocol_projection_share_one_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
        let paths = StoragePaths {
            data_dir: data_dir.to_path_buf(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("open sqlite");
        store.migrate().expect("migrate sqlite");
        let project_repo = store.project_repo();
        let session_repo = store.session_repo();
        let protocol_store = store.protocol_event_store();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let project_id = ProjectId::new();
            let root = Utf8Path::new("C:/workspace/lifecycle-bundle");
            project_repo
                .upsert_project(project_id, root, "Lifecycle Bundle", "none")
                .await
                .expect("insert project");
            let session = session_repo
                .create_session(NewSession {
                    project_id,
                    title: "terminal bundle".to_string(),
                    cwd: root.to_path_buf(),
                    model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                    base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                    access_mode: crate::config::AccessMode::Default,
                })
                .await
                .expect("insert session");
            let turn_id = TurnId::new();
            let (assistant, start_event) = session_repo
                .append_assistant_message_with_protocol_start(
                    NewMessage {
                        session_id: session.id,
                        parent_message_id: None,
                        role: MessageRole::Assistant,
                        metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                            model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                            base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                            finish_reason: None,
                            token_usage: None,
                            summary: false,
                        }),
                    },
                    turn_id,
                    Some(0),
                    STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                )
                .await
                .expect("append assistant start");
            assert!(matches!(start_event, RunEvent::AssistantStarted { .. }));

            let terminal_event = RunEvent::SessionCompleted {
                session_id: session.id,
                finish_reason: Some(FinishReason::Stop),
            };
            session_repo
                .update_message_metadata_and_status_with_protocol_event(
                    session.id,
                    assistant.id,
                    &MessageMetadata::Assistant(AssistantMessageMeta {
                        model: STORAGE_REPOSITORY_FIXTURE_MODEL.to_string(),
                        base_url: STORAGE_REPOSITORY_FIXTURE_BASE_URL.to_string(),
                        finish_reason: Some(FinishReason::Stop),
                        token_usage: None,
                        summary: false,
                    }),
                    SessionStatus::Completed,
                    &terminal_event,
                    turn_id,
                    Some(1),
                )
                .await
                .expect("update terminal bundle");

            let stored_session = session_repo.get_session(session.id).await.expect("session");
            assert_eq!(stored_session.status, SessionStatus::Completed);
            let transcript = session_repo
                .compatibility_transcript(session.id)
                .await
                .expect("transcript");
            let metadata = transcript
                .messages
                .iter()
                .find(|message| message.record.id == assistant.id)
                .map(|message| message.record.metadata.clone())
                .expect("assistant message");
            assert!(matches!(
                metadata,
                MessageMetadata::Assistant(AssistantMessageMeta {
                    finish_reason: Some(FinishReason::Stop),
                    ..
                })
            ));
            let runtime_events = protocol_store
                .list_runtime_events(session.id, turn_id)
                .expect("runtime events");
            assert!(
                runtime_events
                    .iter()
                    .any(|event| matches!(event.msg, RuntimeEventMsg::AssistantStarted { .. }))
            );
            assert!(
                runtime_events
                    .iter()
                    .any(|event| matches!(event.msg, RuntimeEventMsg::TurnCompleted { .. }))
            );
        });
    }

    #[test]
    fn storage_repository_fixtures_use_current_provider_profile() {
        assert!(super::storage_repository_current_provider_profile_fixture_passes());
    }

    #[test]
    fn session_archive_search_lifecycle_is_non_destructive_metadata() {
        assert!(super::session_archive_search_lifecycle_fixture_passes());
    }
}
