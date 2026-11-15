use super::*;

impl SqliteSessionRepository {
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
}

pub(super) fn insert_agent_mailbox_message_in_transaction(
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

pub(super) fn deliver_claimed_explicit_agent_wake_in_transaction(
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

pub(super) fn deliver_all_pending_agent_mail_in_transaction(
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
