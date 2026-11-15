use super::*;

impl SqliteSessionRepository {
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

pub(super) fn agent_trigger_turn_claim_in_connection(
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

pub(super) fn agent_trigger_history_item_for_turn_in_connection(
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

pub(super) fn discard_pending_explicit_agent_wake_in_transaction(
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

pub(super) fn owner_resume_claimed_turn_in_connection(
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

pub(super) fn retained_agent_parent_in_connection(
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

pub(super) fn insert_deferred_agent_completion_in_transaction(
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

pub(super) fn supersede_pending_deferred_completion_in_transaction(
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

pub(super) fn discard_pending_crash_deferred_completion_in_transaction(
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

pub(super) fn pending_direct_child_result_terminal_in_connection(
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

pub(super) fn discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
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

pub(super) fn release_quiescent_deferred_completions_after_interruption_in_transaction(
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

pub(super) fn truncate_agent_completion_middle(value: &str, max_tokens: usize) -> String {
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

pub(super) fn deferred_agent_completion_in_connection(
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

pub(super) fn has_pending_owner_resume_requests_in_connection(
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

pub(super) fn resolve_claimed_owner_resume_requests_in_transaction(
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

pub(super) fn repend_claimed_owner_resume_requests_in_transaction(
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

pub(super) fn settle_pending_agent_trigger_in_transaction(
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
    if agent_trigger_turn_claim_in_connection(transaction, session_id, expected_history_item_id)?
        .is_some()
    {
        return Ok(PendingAgentTriggerSettlement::WakeOwnedOrResolved);
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

pub(super) fn settle_pending_owner_resume_in_transaction(
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

/// Admission gate of the child-agent lifecycle. `false` means the turn must not be
/// admitted now: a deferred completion still waits for its owner (unless this turn is the
/// owner resume or explicit trigger that recovers a crash), the expected trigger was
/// already claimed, or the expected owner resume is not the next schedulable one.
pub(super) fn agent_work_admissible(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    expected_agent_trigger: Option<HistoryItemId>,
    expected_owner_resume: Option<OwnerResumeRequestId>,
) -> Result<bool, StorageError> {
    let pending_deferred =
        deferred_agent_completion_in_connection(transaction, session_id, None, Some("pending"))?;
    let pending_crash = pending_deferred
        .as_ref()
        .is_some_and(|deferred| deferred.kind == DeferredAgentCompletionKind::CrashFailed);
    let owner_resume_can_recover_crash = expected_owner_resume.is_some() && pending_crash;
    let explicit_trigger_can_recover_crash = expected_agent_trigger.is_some() && pending_crash;
    if pending_deferred.is_some()
        && !owner_resume_can_recover_crash
        && !explicit_trigger_can_recover_crash
    {
        return Ok(false);
    }
    if let Some(expected_history_item_id) = expected_agent_trigger
        && !pending_agent_trigger_is_unclaimed_in_transaction(
            transaction,
            session_id,
            expected_history_item_id,
            false,
        )?
    {
        return Ok(false);
    }
    if let Some(expected_request_id) = expected_owner_resume
        && schedulable_owner_resume_request_id_in_connection(transaction, session_id)?
            != Some(expected_request_id)
    {
        return Ok(false);
    }
    Ok(true)
}

/// Bind the expected trigger and owner resume to the newly admitted turn, after
/// SessionStarted and in the same transaction as the admission.
pub(super) fn claim_agent_work_for_admitted_turn(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    expected_agent_trigger: Option<HistoryItemId>,
    expected_owner_resume: Option<OwnerResumeRequestId>,
    now_ms: i64,
) -> Result<(), StorageError> {
    if let Some(expected_history_item_id) = expected_agent_trigger {
        insert_agent_trigger_turn_claim_in_transaction(
            transaction,
            session_id,
            admission_id,
            turn_id,
            expected_history_item_id,
            now_ms,
        )?;
    }
    if let Some(expected_request_id) = expected_owner_resume {
        let claimed = claim_pending_owner_resume_requests_in_transaction(
            transaction,
            session_id,
            turn_id,
            expected_request_id,
            now_ms,
        )?;
        if claimed == 0 {
            return Err(StorageError::Message(format!(
                "owner-resume admission for session {session_id} lost request {expected_request_id} after SessionStarted"
            )));
        }
    } else if expected_agent_trigger.is_some()
        && let Some(pending_request_id) =
            oldest_pending_owner_resume_request_id_in_connection(transaction, session_id)?
    {
        // Explicit owner work wins the wake projection, but that same newly-admitted turn
        // resumes ownership and therefore coalesces every already-pending resume request.
        claim_pending_owner_resume_requests_in_transaction(
            transaction,
            session_id,
            turn_id,
            pending_request_id,
            now_ms,
        )?;
    }
    Ok(())
}

/// What a recorded turn terminal leaves for its parent. Built by the turn terminal and by
/// expired-run recovery, which decide `deferred` and `owner_resume_pending` themselves.
pub(super) struct TerminalHandoff {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tree_stop_fenced: bool,
    /// The direct parent and kind when this terminal must wait for its owner.
    pub deferred: Option<(SessionId, DeferredAgentCompletionKind)>,
    pub pending_direct_child_result: Option<RuntimeEventId>,
    pub owner_resume_pending: bool,
    /// Names the root lookup in an error, so each caller keeps its message.
    pub root_label: &'static str,
    pub now_ms: i64,
}

/// After a turn terminal is recorded: defer the result for its owner, or hand it to the
/// parent. A tree-stop-fenced terminal does neither. `terminal` is read only for the
/// handoff, as before.
pub(super) fn publish_or_defer_terminal_handoff<'a>(
    transaction: &Transaction<'_>,
    handoff: TerminalHandoff,
    terminal: impl FnOnce() -> Result<&'a DurableTurnTerminal, StorageError>,
) -> Result<(), StorageError> {
    let TerminalHandoff {
        session_id,
        turn_id,
        tree_stop_fenced,
        deferred,
        pending_direct_child_result,
        owner_resume_pending,
        root_label,
        now_ms,
    } = handoff;
    if tree_stop_fenced {
        return Ok(());
    }
    if let Some((parent_session_id, kind)) = deferred {
        insert_deferred_agent_completion_in_transaction(
            transaction,
            session_id,
            turn_id,
            parent_session_id,
            kind,
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
                parse_session_id_text(&root_session_id, root_label)?,
                now_ms,
            )?;
        }
    } else if !owner_resume_pending {
        append_agent_completion_handoff_in_transaction(
            transaction,
            session_id,
            turn_id,
            terminal()?,
            now_ms,
        )?;
    }
    Ok(())
}
