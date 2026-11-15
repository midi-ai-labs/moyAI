use super::*;

impl SqliteSessionRepository {
    pub async fn delete_session_tree(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<SessionId>, StorageError> {
        self.delete_session_tree_with_revisions(session_id, None)
            .await
    }

    pub(crate) async fn delete_session_tree_checked(
        &self,
        session_id: SessionId,
        expected_revisions: &[(SessionId, u64)],
    ) -> Result<Vec<SessionId>, StorageError> {
        self.delete_session_tree_with_revisions(session_id, Some(expected_revisions))
            .await
    }

    async fn delete_session_tree_with_revisions(
        &self,
        session_id: SessionId,
        expected_revisions: Option<&[(SessionId, u64)]>,
    ) -> Result<Vec<SessionId>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(expected_revisions) = expected_revisions {
            let actual = origin_history_revisions_for_tree_in_connection(
                &transaction,
                session_id,
                expected_revisions.len(),
            )?;
            verify_origin_history_revisions(actual, expected_revisions)?;
        }
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

    pub async fn rollback_session_transaction(
        &self,
        session_id: SessionId,
        num_turns: usize,
    ) -> Result<crate::session::SessionRollbackResult, StorageError> {
        self.rollback_session_transaction_with_revisions(session_id, num_turns, None)
            .await
    }

    pub(crate) async fn rollback_session_transaction_checked(
        &self,
        session_id: SessionId,
        num_turns: usize,
        expected_revisions: &[(SessionId, u64)],
    ) -> Result<crate::session::SessionRollbackResult, StorageError> {
        self.rollback_session_transaction_with_revisions(
            session_id,
            num_turns,
            Some(expected_revisions),
        )
        .await
    }

    async fn rollback_session_transaction_with_revisions(
        &self,
        session_id: SessionId,
        num_turns: usize,
        expected_revisions: Option<&[(SessionId, u64)]>,
    ) -> Result<crate::session::SessionRollbackResult, StorageError> {
        if num_turns == 0 {
            return Err(StorageError::Message(
                "session rollback turn count must be greater than zero".to_string(),
            ));
        }
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(expected_revisions) = expected_revisions {
            let actual = origin_history_revisions_for_tree_in_connection(
                &transaction,
                session_id,
                expected_revisions.len(),
            )?;
            verify_origin_history_revisions(actual, expected_revisions)?;
        }

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
            delete_turn_owned_harness_rows(&transaction, session_id, *turn_id)?;
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
                 model_parameters_json, provider_connection_json,
                 created_at_ms, updated_at_ms, completed_at_ms
             )
             SELECT ?2, project_id, ?3, 'idle', cwd_path, model_name, base_url, access_mode,
                     model_parameters_json, provider_connection_json, ?4, ?4, NULL
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

    pub(crate) async fn fork_latest_turn_for_edit_checked(
        &self,
        source_session_id: SessionId,
        expected_turn_id: TurnId,
        expected_admission_revision: u64,
        expected_revisions: &[(SessionId, u64)],
    ) -> Result<SessionEditForkResult, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual = origin_history_revisions_for_tree_in_connection(
            &transaction,
            source_session_id,
            expected_revisions.len(),
        )?;
        verify_origin_history_revisions(actual, expected_revisions)?;
        let source = session_record_from_connection(&transaction, source_session_id)?;
        let current_revision =
            session_admission_revision_in_connection(&transaction, source_session_id)?;
        let latest_turn_id =
            latest_protocol_turn_ids_in_transaction(&transaction, source_session_id, 1)?
                .into_iter()
                .next();
        if current_revision != expected_admission_revision
            || latest_turn_id != Some(expected_turn_id)
        {
            return Err(StorageError::Message(
                "the latest message changed; refresh this chat before editing".into(),
            ));
        }
        let terminal =
            terminal_for_turn_in_connection(&transaction, source_session_id, expected_turn_id)?;
        let terminal_matches_status = match (source.status, terminal.map(|item| item.outcome)) {
            (SessionStatus::Completed, Some(TurnTerminalOutcome::Completed))
            | (SessionStatus::Failed, Some(TurnTerminalOutcome::Failed { .. }))
            | (
                SessionStatus::Cancelled,
                Some(TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                }),
            ) => true,
            _ => false,
        };
        if !terminal_matches_status {
            return Err(StorageError::Message(
                "only the latest finished or user-stopped message can be edited".into(),
            ));
        }
        if active_session_for_mutation_branch(&transaction, source_session_id, true)?.is_some() {
            return Err(StorageError::Message(
                "stop this chat's active work before editing its message".into(),
            ));
        }
        let first_turn_position: i64 = transaction
            .query_row(
                "SELECT MIN(append_position) FROM protocol_item_append_order
             WHERE session_id = ?1 AND scope_kind = 'turn' AND turn_id = ?2",
                params![source_session_id.to_string(), expected_turn_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .ok_or_else(|| {
                StorageError::Message("the latest message has no canonical turn".into())
            })?;
        let trailing_session_items = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM protocol_item_append_order
             WHERE session_id = ?1 AND scope_kind = 'session' AND append_position > ?2)",
            params![source_session_id.to_string(), first_turn_position],
            |row| row.get::<_, bool>(0),
        )?;
        if trailing_session_items {
            return Err(StorageError::Message(
                "this chat has later session changes that cannot be safely omitted from the edit"
                    .into(),
            ));
        }
        let mut statement = transaction.prepare(
            "SELECT payload_json FROM protocol_history_items
             WHERE session_id = ?1 AND turn_id = ?2
               AND json_extract(payload_json, '$.kind') IN ('user_turn', 'steer_turn')
             ORDER BY sequence_no LIMIT 2",
        )?;
        let user_rows = statement
            .query_map(
                params![source_session_id.to_string(), expected_turn_id.to_string()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if user_rows.len() != 1 {
            return Err(StorageError::Message(
                "this turn has multiple user messages; edit them in a new request".into(),
            ));
        }
        let HistoryItemPayload::UserTurn { content, .. } =
            serde_json::from_str::<HistoryItemPayload>(&user_rows[0])?
        else {
            return Err(StorageError::Message(
                "only the initial user message of the latest turn can be edited".into(),
            ));
        };
        let mut text_parts = Vec::with_capacity(content.len());
        let mut retained_images = Vec::new();
        for part in content {
            match part {
                ContentPart::Text { text } => text_parts.push(text),
                ContentPart::Image { image } => retained_images.push(image),
            }
        }
        crate::llm::image_validation::validate_retained_images(&retained_images)
            .map_err(StorageError::Message)?;
        let editable_text = text_parts.join("\n");
        if editable_text.trim().is_empty() {
            return Err(StorageError::Message(
                "the latest message has no editable text".into(),
            ));
        }

        let target_session_id = SessionId::new();
        let now = SystemClock::now_ms();
        let title = format!("{}（編集）", source.title);
        transaction.execute(
            "INSERT INTO sessions (
                 id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                 model_parameters_json, provider_connection_json,
                 created_at_ms, updated_at_ms, completed_at_ms
             )
             SELECT ?2, project_id, ?3, 'idle', cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, provider_connection_json, ?4, ?4, NULL
             FROM sessions WHERE id = ?1",
            params![
                source_session_id.to_string(),
                target_session_id.to_string(),
                title,
                now
            ],
        )?;
        fork_canonical_items_in_transaction(&transaction, source_session_id, target_session_id)?;
        // The existing canonical fork rewrites history identities and preserves compaction
        // references. Remove every projection of the edited turn only in the new branch.
        for table in [
            "turn_steer_inputs",
            "protocol_turn_items",
            "protocol_history_items",
            "protocol_runtime_events",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ] {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE session_id = ?1 AND turn_id = ?2"),
                params![target_session_id.to_string(), expected_turn_id.to_string()],
            )?;
        }
        let forked_session = session_record_from_connection(&transaction, target_session_id)?;
        transaction.commit()?;
        Ok(SessionEditForkResult {
            source_session: source,
            forked_session,
            edited_turn_id: expected_turn_id,
            editable_text,
            retained_images,
        })
    }
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

pub(crate) fn delete_session_rows(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<(), StorageError> {
    let owned_side_conversation_id = transaction
        .query_row(
            "SELECT conversation_session_id
             FROM side_chat_bindings
             WHERE owner_session_id = ?1",
            [session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| parse_session_id_text(&value, "owned side chat conversation"))
        .transpose()?;
    if let Some(conversation_session_id) = owned_side_conversation_id {
        match session_stop_state_from_connection(transaction, conversation_session_id)? {
            Some(DurableSessionStopState::Running(_)) => {
                return Err(StorageError::Message(format!(
                    "session {session_id} owns active side chat conversation {conversation_session_id}; stop it before deleting the owner"
                )));
            }
            Some(DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_)) => {}
            None => {
                return Err(StorageError::Message(format!(
                    "session {session_id} owns missing side chat conversation {conversation_session_id}"
                )));
            }
        }
        transaction.execute(
            "DELETE FROM side_chat_bindings
             WHERE owner_session_id = ?1 AND conversation_session_id = ?2",
            params![session_id.to_string(), conversation_session_id.to_string()],
        )?;
        delete_session_rows(transaction, conversation_session_id)?;
    }
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

fn delete_turn_owned_harness_rows(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<(), StorageError> {
    let session_id = session_id.to_string();
    let turn_id = turn_id.to_string();
    for table in [
        "harness_replay_reports",
        "harness_gate_results",
        "harness_contracts",
        "harness_artifacts",
        "harness_events",
    ] {
        transaction.execute(
            &format!(
                "DELETE FROM {table}
                 WHERE run_id IN (
                     SELECT id
                     FROM harness_runs
                     WHERE session_id = ?1 AND protocol_turn_id = ?2
                 )"
            ),
            params![session_id, turn_id],
        )?;
    }
    transaction.execute(
        "DELETE FROM harness_runs
         WHERE session_id = ?1 AND protocol_turn_id = ?2",
        params![session_id, turn_id],
    )?;
    Ok(())
}
