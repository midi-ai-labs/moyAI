use super::*;

impl SqliteSessionRepository {
    pub async fn accept_active_turn_steer(
        &self,
        session_id: SessionId,
        expected_admission_revision: u64,
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
        let actual_admission_revision =
            session_admission_revision_in_connection(&transaction, session_id)?;
        if turn_started_before_applicable_tree_stop_fence_in_transaction(
            &transaction,
            session_id,
            active_turn_id,
        )? {
            return Err(StorageError::Message(format!(
                "active turn {active_turn_id} for session {session_id} was closed by a durable tree Stop"
            )));
        }
        if active_turn_id != steer.expected_turn_id
            || actual_admission_revision != expected_admission_revision
        {
            return Err(StorageError::Message(format!(
                "expected active turn `{}` at revision {expected_admission_revision}, but current active turn is `{active_turn_id}` at revision {actual_admission_revision}",
                steer.expected_turn_id,
            )));
        }

        let input_id = HistoryItemId::new();
        let payload_json = serde_json::to_string(&HistoryItemPayload::SteerTurn {
            expected_turn_id: active_turn_id,
            content: steer.content_parts(),
            additional_context: steer.additional_context.clone(),
            client_user_message_id: steer.client_user_message_id.clone(),
        })?;
        let payload_sha256 = sha256_payload(&payload_json);
        transaction.execute(
            "INSERT INTO turn_steer_inputs (
                 id,
                 session_id,
                 admission_id,
                 turn_id,
                 payload_json,
                 payload_sha256,
                 origin_kind,
                 state,
                 delivered_history_item_id,
                 resolved_by_terminal_event_id,
                 accepted_at_ms,
                 delivered_at_ms,
                 discarded_at_ms,
                 updated_at_ms
             )
             VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, 'runtime', 'queued',
                 NULL, NULL, ?7, NULL, NULL, ?7
             )",
            params![
                input_id.to_string(),
                session_id.to_string(),
                durable_admission.admission_id.to_string(),
                active_turn_id.to_string(),
                payload_json,
                payload_sha256,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(input_id)
    }

    pub(crate) fn deliver_all_pending_turn_steers_for_admitted_turn(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<Vec<HistoryItemId>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(&transaction, session_id, admission_id, turn_id)?;
        let delivered = deliver_all_pending_turn_steers_in_transaction(
            &transaction,
            session_id,
            admission_id,
            turn_id,
            now,
        )?;
        transaction.commit()?;
        Ok(delivered)
    }

    pub(crate) fn has_pending_turn_steers_for_admitted_turn(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<bool, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(&transaction, session_id, admission_id, turn_id)?;
        let pending = count_pending_turn_steers_in_transaction(
            &transaction,
            session_id,
            admission_id,
            turn_id,
        )? > 0;
        transaction.commit()?;
        Ok(pending)
    }
}

struct ParsedTurnSteerPayload {
    content: Vec<ContentPart>,
    additional_context: BTreeMap<String, AdditionalContextEntry>,
    client_user_message_id: Option<String>,
}

fn parse_durable_turn_steer_payload(
    input_id: HistoryItemId,
    expected_turn_id: TurnId,
    payload_json: &str,
    payload_sha256: &str,
) -> Result<ParsedTurnSteerPayload, StorageError> {
    if sha256_payload(payload_json) != payload_sha256 {
        return Err(StorageError::Message(format!(
            "queued turn steer {input_id} payload hash does not match its durable bytes"
        )));
    }
    let payload = serde_json::from_str::<HistoryItemPayload>(payload_json)?;
    let HistoryItemPayload::SteerTurn {
        expected_turn_id: payload_turn_id,
        content,
        additional_context,
        client_user_message_id,
    } = payload
    else {
        return Err(StorageError::Message(format!(
            "queued turn steer {input_id} does not contain SteerTurn payload"
        )));
    };
    if payload_turn_id != expected_turn_id {
        return Err(StorageError::Message(format!(
            "queued turn steer {input_id} targets {payload_turn_id}, not turn {expected_turn_id}"
        )));
    }
    Ok(ParsedTurnSteerPayload {
        content,
        additional_context,
        client_user_message_id,
    })
}

pub(super) fn pending_turn_input_projections_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    runtime_state: ValidatedSessionRuntimeState,
) -> Result<Vec<crate::session::PendingTurnInputProjection>, StorageError> {
    let Some(admission) = runtime_state
        .admission
        .filter(|_| runtime_state.status == SessionStatus::Running)
    else {
        return Ok(Vec::new());
    };
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT input.id,
                    input.turn_id,
                    input.payload_json,
                    input.payload_sha256,
                    input.accepted_at_ms
             FROM turn_steer_inputs AS input
             INNER JOIN turn_steer_input_enqueue_order AS enqueue
               ON enqueue.input_id = input.id
              AND enqueue.session_id = input.session_id
              AND enqueue.turn_id = input.turn_id
             WHERE input.session_id = ?1
               AND input.admission_id = ?2
               AND input.turn_id = ?3
               AND input.origin_kind = 'runtime'
               AND input.state = 'queued'
             ORDER BY enqueue.enqueue_position ASC",
        )?;
        statement
            .query_map(
                params![
                    session_id.to_string(),
                    admission.admission_id.to_string(),
                    admission.turn_id.to_string(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    rows.into_iter()
        .map(
            |(input_id, turn_id, payload_json, payload_sha256, accepted_at_ms)| {
                let input_id = input_id.parse::<HistoryItemId>().map_err(|error| {
                    StorageError::Message(format!(
                        "pending turn input has invalid identity `{input_id}`: {error}"
                    ))
                })?;
                let turn_id = turn_id.parse::<TurnId>().map_err(|error| {
                    StorageError::Message(format!(
                        "pending turn input {input_id} has invalid turn `{turn_id}`: {error}"
                    ))
                })?;
                if turn_id != admission.turn_id {
                    return Err(StorageError::Message(format!(
                        "pending turn input {input_id} belongs to {turn_id}, not active turn {}",
                        admission.turn_id
                    )));
                }
                let parsed = parse_durable_turn_steer_payload(
                    input_id,
                    turn_id,
                    &payload_json,
                    &payload_sha256,
                )?;
                let image_count = parsed
                    .content
                    .iter()
                    .filter(|part| matches!(part, ContentPart::Image { .. }))
                    .count();
                Ok(crate::session::PendingTurnInputProjection {
                    id: input_id,
                    turn_id,
                    text: content_parts_text(&parsed.content),
                    image_count,
                    accepted_at_ms,
                    client_user_message_id: parsed.client_user_message_id,
                })
            },
        )
        .collect()
}

fn count_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
) -> Result<usize, StorageError> {
    let count = transaction.query_row(
        "SELECT COUNT(*)
         FROM turn_steer_inputs
         WHERE session_id = ?1
           AND admission_id = ?2
           AND turn_id = ?3
           AND origin_kind = 'runtime'
           AND state = 'queued'",
        params![
            session_id.to_string(),
            admission_id.to_string(),
            turn_id.to_string(),
        ],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).map_err(|_| {
        StorageError::Message(format!(
            "negative pending turn-steer count for session {session_id} turn {turn_id}"
        ))
    })
}

fn deliver_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    limit: usize,
    now_ms: i64,
) -> Result<DeliveredTurnSteerPage, StorageError> {
    let query_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(129);
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT input.id, input.payload_json, input.payload_sha256
             FROM turn_steer_inputs AS input
             INNER JOIN turn_steer_input_enqueue_order AS enqueue
               ON enqueue.input_id = input.id
              AND enqueue.session_id = input.session_id
              AND enqueue.turn_id = input.turn_id
             WHERE input.session_id = ?1
               AND input.admission_id = ?2
               AND input.turn_id = ?3
               AND input.origin_kind = 'runtime'
               AND input.state = 'queued'
             ORDER BY enqueue.enqueue_position ASC
             LIMIT ?4",
        )?;
        statement
            .query_map(
                params![
                    session_id.to_string(),
                    admission_id.to_string(),
                    turn_id.to_string(),
                    query_limit,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    let has_more = rows.len() > limit;
    let mut history_item_ids = Vec::with_capacity(rows.len().min(limit));
    for (input_id, payload_json, payload_sha256) in rows.into_iter().take(limit) {
        let input_id = input_id.parse::<HistoryItemId>().map_err(|error| {
            StorageError::Message(format!(
                "queued turn steer has invalid identity `{input_id}`: {error}"
            ))
        })?;
        let ParsedTurnSteerPayload {
            content,
            additional_context,
            client_user_message_id,
        } = parse_durable_turn_steer_payload(input_id, turn_id, &payload_json, &payload_sha256)?;
        let history_item = HistoryItem {
            id: input_id,
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 0,
            created_at_ms: now_ms,
            payload: HistoryItemPayload::SteerTurn {
                expected_turn_id: turn_id,
                content: content.clone(),
                additional_context,
                client_user_message_id: client_user_message_id.clone(),
            },
        };
        let turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: Some(input_id),
            sequence_no: 0,
            payload: TurnItemPayload::SteerMessage {
                text: content_parts_text(&content),
            },
        };
        let event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: now_ms,
            msg: RuntimeEventMsg::SteerInputAccepted {
                item_count: content.len(),
                client_user_message_id,
            },
        };
        let stored = insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            transaction,
            &event,
            Some(&history_item),
            Some(&turn_item),
        )?;
        if stored.history_item.as_ref().map(|item| item.id) != Some(input_id) {
            return Err(StorageError::Message(format!(
                "turn-steer delivery for {input_id} did not preserve canonical history identity"
            )));
        }
        let transitioned = transaction.execute(
            "UPDATE turn_steer_inputs
             SET state = 'delivered',
                 delivered_history_item_id = id,
                 delivered_at_ms = ?4,
                 updated_at_ms = MAX(updated_at_ms, ?4)
             WHERE id = ?1
               AND session_id = ?2
               AND admission_id = ?3
               AND state = 'queued'",
            params![
                input_id.to_string(),
                session_id.to_string(),
                admission_id.to_string(),
                now_ms,
            ],
        )?;
        if transitioned != 1 {
            return Err(StorageError::Message(format!(
                "turn steer {input_id} lost its queued delivery owner"
            )));
        }
        history_item_ids.push(input_id);
    }
    Ok(DeliveredTurnSteerPage {
        history_item_ids,
        has_more,
    })
}

pub(super) fn deliver_all_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    now_ms: i64,
) -> Result<Vec<HistoryItemId>, StorageError> {
    let mut delivered = Vec::new();
    loop {
        let page = deliver_pending_turn_steers_in_transaction(
            transaction,
            session_id,
            admission_id,
            turn_id,
            128,
            now_ms,
        )?;
        delivered.extend(page.history_item_ids);
        if !page.has_more {
            return Ok(delivered);
        }
    }
}

pub(super) fn discard_all_pending_turn_steers_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    terminal_event_id: RuntimeEventId,
    now_ms: i64,
) -> Result<usize, StorageError> {
    let discarded = transaction.execute(
        "UPDATE turn_steer_inputs
         SET state = 'discarded',
             resolved_by_terminal_event_id = ?4,
             discarded_at_ms = ?5,
             updated_at_ms = MAX(updated_at_ms, ?5)
         WHERE session_id = ?1
           AND admission_id = ?2
           AND turn_id = ?3
           AND origin_kind = 'runtime'
           AND state = 'queued'",
        params![
            session_id.to_string(),
            admission_id.to_string(),
            turn_id.to_string(),
            terminal_event_id.to_string(),
            now_ms,
        ],
    )?;
    Ok(discarded)
}
