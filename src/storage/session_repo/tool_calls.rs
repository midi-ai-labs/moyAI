use super::*;

impl SqliteSessionRepository {
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

pub(super) fn validate_canonical_tool_call_in_transaction(
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
