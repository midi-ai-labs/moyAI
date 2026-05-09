use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;
use crate::runtime::{Clock, SystemClock};
use crate::session::{
    CompletionState, FailureKind, FailureState, MessageId, MessageMetadata, MessageRecord,
    MessageRole, NewMessage, NewPart, NewSession, PartKind, PartRecord, ProcessPhase, SessionId,
    SessionRecord, SessionRepository, SessionStateSnapshot, SessionStatus, TaskRoute, TodoItem,
    TodoKind, TodoPriority, TodoStatus, ToolCallId, ToolCallRecord, ToolCallStatus, Transcript,
    TranscriptMessage, VerificationState,
};

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
}

#[async_trait(?Send)]
impl SessionRepository for SqliteSessionRepository {
    async fn create_session(&self, draft: NewSession) -> Result<SessionRecord, StorageError> {
        let id = SessionId::new();
        let now = SystemClock.now_ms();
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO sessions (id, project_id, title, status, cwd_path, model_name, base_url, created_at_ms, updated_at_ms, completed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
            params![
                id.to_string(),
                draft.project_id.to_string(),
                draft.title,
                "idle",
                draft.cwd.as_str(),
                draft.model,
                draft.base_url,
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
            "SELECT project_id, title, status, cwd_path, model_name, base_url, created_at_ms, updated_at_ms, completed_at_ms
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
                    created_at_ms: row.get(6)?,
                    updated_at_ms: row.get(7)?,
                    completed_at_ms: row.get(8)?,
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
                "SELECT id FROM sessions WHERE project_id = ?1 ORDER BY updated_at_ms DESC LIMIT 1",
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
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id FROM sessions WHERE project_id = ?1 ORDER BY updated_at_ms DESC LIMIT ?2",
        )?;
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

    async fn set_status(&self, id: SessionId, status: SessionStatus) -> Result<(), StorageError> {
        let now = SystemClock.now_ms();
        let status_text = match status {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Completed => "completed",
            SessionStatus::AwaitingUser => "awaiting_user",
            SessionStatus::Failed => "failed",
        };
        let completed_at_ms = if matches!(
            status,
            SessionStatus::Completed | SessionStatus::AwaitingUser | SessionStatus::Failed
        ) {
            Some(now)
        } else {
            None
        };
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "UPDATE sessions SET status = ?2, updated_at_ms = ?3, completed_at_ms = ?4 WHERE id = ?1",
            params![id.to_string(), status_text, now, completed_at_ms],
        )?;
        Ok(())
    }

    async fn append_message(
        &self,
        draft: NewMessage,
        parts: Vec<NewPart>,
    ) -> Result<MessageRecord, StorageError> {
        let id = MessageId::new();
        let now = SystemClock.now_ms();
        let sequence_no = next_message_sequence(&self.connection, draft.session_id)?;
        let metadata_json = serde_json::to_string(&draft.metadata)?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
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
        drop(connection);

        let record = MessageRecord {
            id,
            session_id: draft.session_id,
            role: draft.role,
            parent_message_id: draft.parent_message_id,
            sequence_no,
            created_at_ms: now,
            metadata: draft.metadata,
        };

        for part in parts {
            self.append_part(id, part).await?;
        }

        Ok(record)
    }

    async fn append_part(
        &self,
        message_id: MessageId,
        part: NewPart,
    ) -> Result<PartRecord, StorageError> {
        let id = crate::session::PartId::new();
        let now = SystemClock.now_ms();
        let sequence_no = next_part_sequence(&self.connection, message_id)?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
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

    async fn transcript(&self, session_id: SessionId) -> Result<Transcript, StorageError> {
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

    async fn get_state(&self, session_id: SessionId) -> Result<SessionStateSnapshot, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let row = connection
            .query_row(
                "SELECT task_route, phase, review_scope_json, active_todo_id, active_targets_json, contract_refs_json, failure_kind, failure_summary, failure_tool_name, failure_targets_json,
                        verification_todo_id, verification_commands_json, verification_failures_json, verification_evidence_summary,
                        completion_closeout_ready, completion_open_work_count, completion_verification_pending, completion_route_contract_pending,
                        completion_blocked_reason, completion_route_contract_summary, docs_route_state_json, implementation_handoff_json,
                        verification_failure_cluster_json, verification_requirement_refs_json
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

    async fn update_state(
        &self,
        session_id: SessionId,
        state: &SessionStateSnapshot,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        upsert_session_state_row(&connection, session_id, state, SystemClock::now_ms())
    }

    async fn update_todos(
        &self,
        session_id: SessionId,
        todos: &[TodoItem],
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![session_id.to_string()],
        )?;
        for (position, todo) in todos.iter().enumerate() {
            connection.execute(
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
        "failed" => SessionStatus::Failed,
        _ => SessionStatus::Failed,
    }
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

fn next_message_sequence(
    connection: &Arc<Mutex<Connection>>,
    session_id: SessionId,
) -> Result<i64, StorageError> {
    let connection = connection.lock().expect("sqlite mutex poisoned");
    let value: Option<i64> = connection.query_row(
        "SELECT MAX(sequence_no) FROM messages WHERE session_id = ?1",
        params![session_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(value.unwrap_or(0) + 1)
}

fn next_part_sequence(
    connection: &Arc<Mutex<Connection>>,
    message_id: MessageId,
) -> Result<i64, StorageError> {
    let connection = connection.lock().expect("sqlite mutex poisoned");
    let value: Option<i64> = connection.query_row(
        "SELECT MAX(sequence_no) FROM message_parts WHERE message_id = ?1",
        params![message_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(value.unwrap_or(0) + 1)
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
    connection.execute(
        "INSERT INTO session_state (
             session_id, task_route, phase, review_scope_json, active_todo_id, active_targets_json, contract_refs_json, failure_kind, failure_summary, failure_tool_name,
             failure_targets_json, verification_todo_id, verification_commands_json, verification_failures_json,
             verification_evidence_summary, completion_closeout_ready, completion_open_work_count,
             completion_verification_pending, completion_route_contract_pending, completion_blocked_reason, completion_route_contract_summary,
             docs_route_state_json, implementation_handoff_json, verification_failure_cluster_json, verification_requirement_refs_json, updated_at_ms
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
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
            updated_at_ms
        ],
    )?;
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
