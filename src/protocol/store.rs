use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::error::StorageError;
use crate::protocol::{
    HistoryItem, HistoryItemId, HistoryItemPayload, ModeKind, RuntimeEvent, RuntimeEventId,
    RuntimeEventMsg, SubAgentActivityKind, TurnId, TurnItem, TurnItemId, TurnItemPayload,
    compacted_history_item_ids, project_sub_agent_activity,
};
use crate::runtime::SystemClock;
use crate::session::SessionId;
use crate::storage::session_repo::{SessionProtocolWriteAuthority, normalize_run_lease_now_ms};

pub trait ProtocolEventStore {
    fn list_runtime_events(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<RuntimeEvent>, StorageError>;
    fn list_runtime_events_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<RuntimeEvent>, StorageError>;
    fn list_history_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<HistoryItem>, StorageError>;
    fn list_history_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<HistoryItem>, StorageError>;
    fn collaboration_mode_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<ModeKind, StorageError>;
    /// Appends a typed mode instruction when `mode` differs from the latest
    /// canonical value. Returns `None` for a same-value update.
    fn set_collaboration_mode(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        mode: ModeKind,
    ) -> Result<Option<HistoryItem>, StorageError>;
    fn list_turn_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<TurnItem>, StorageError>;
    fn list_turn_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TurnItem>, StorageError>;
    fn latest_turn_position_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(TurnId, i64)>, StorageError>;
    fn fork_canonical_items(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<(usize, usize), StorageError>;
    fn fork_agent_context(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<usize, StorageError>;
}

#[derive(Clone)]
pub struct SqliteProtocolEventStore {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredProtocolEventBundle {
    pub runtime_event: RuntimeEvent,
    pub history_item: Option<HistoryItem>,
}

impl SqliteProtocolEventStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    pub(super) fn append_recording_projection_allocating(
        &self,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<StoredProtocolEventBundle, StorageError> {
        validate_recording_projection(event, history_item, turn_item)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = insert_event_bundle_unchecked(&transaction, event, history_item, turn_item)?;
        transaction.commit()?;
        Ok(stored)
    }

    pub(super) fn append_admitted_recording_projection_allocating(
        &self,
        admission_id: &str,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<Option<StoredProtocolEventBundle>, StorageError> {
        self.append_admitted_recording_projection_allocating_at(
            admission_id,
            event,
            history_item,
            turn_item,
            SystemClock::now_ms(),
        )
    }

    pub(super) fn append_admitted_recording_projection_allocating_at(
        &self,
        admission_id: &str,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
        now_ms: i64,
    ) -> Result<Option<StoredProtocolEventBundle>, StorageError> {
        validate_recording_projection(event, history_item, turn_item)?;
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
                    event.session_id.to_string(),
                    admission_id,
                    event.turn_id.to_string(),
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
        let stored = insert_event_bundle_unchecked(&transaction, event, history_item, turn_item)?;
        transaction.commit()?;
        Ok(Some(stored))
    }

    /// Records the runtime/control projection owned by the multi-agent runtime.
    ///
    /// Unlike the recording sink this API cannot accept an arbitrary projection,
    /// so model-response, tool-settlement, and terminal payloads remain reachable
    /// only through their session-repository transactions.
    pub(crate) fn append_sub_agent_activity(
        &self,
        session_id: SessionId,
        activity_id: String,
        agent_session_id: SessionId,
        agent_path: String,
        activity_kind: SubAgentActivityKind,
    ) -> Result<(), StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let turn_id = transaction
            .query_row(
                "SELECT active_turn_id
                 FROM sessions
                 WHERE id = ?1
                   AND status = 'running'
                   AND active_run_id IS NOT NULL
                   AND active_turn_id IS NOT NULL
                   AND active_run_lease_expires_at_ms > ?2",
                params![session_id.to_string(), now],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "session {session_id} has no active turn that can own sub-agent activity"
                ))
            })?
            .parse::<TurnId>()
            .map_err(|error| StorageError::Message(error.to_string()))?;
        let projection = project_sub_agent_activity(
            session_id,
            turn_id,
            0,
            activity_id,
            agent_session_id,
            agent_path,
            activity_kind,
        );
        insert_event_bundle_unchecked(
            &transaction,
            &projection.runtime_event,
            projection.history_item.as_ref(),
            projection.turn_item.as_ref(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seed_runtime_event_for_test(
        &self,
        event: &RuntimeEvent,
    ) -> Result<(), StorageError> {
        self.seed_event_bundle_for_test(event, None, None)
    }

    #[cfg(test)]
    pub(crate) fn seed_event_bundle_for_test(
        &self,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_event_bundle_unchecked(&transaction, event, history_item, turn_item)?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seed_history_item_for_test(
        &self,
        item: &HistoryItem,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence_no = claim_protocol_sequence_in_transaction(
            &transaction,
            item.session_id,
            item.turn_id,
            item.sequence_no,
        )?;
        let mut stored_item = item.clone();
        stored_item.sequence_no = sequence_no;
        insert_history_item(&transaction, &stored_item)?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seed_turn_item_for_test(&self, item: &TurnItem) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence_no = claim_protocol_sequence_in_transaction(
            &transaction,
            item.session_id,
            item.turn_id,
            item.sequence_no,
        )?;
        let mut stored_item = item.clone();
        stored_item.sequence_no = sequence_no;
        insert_turn_item(&transaction, &stored_item)?;
        transaction.commit()?;
        Ok(())
    }
}

impl ProtocolEventStore for SqliteProtocolEventStore {
    fn list_runtime_events(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<RuntimeEvent>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, sequence_no, msg_json, created_at_ms
             FROM protocol_runtime_events
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
        let mut events = Vec::new();
        for row in rows {
            let (id, sequence_no, msg_json, created_at_ms) = row?;
            events.push(RuntimeEvent {
                id: parse_protocol_id::<RuntimeEventId>(&id, "runtime event")?,
                session_id,
                turn_id,
                sequence_no,
                created_at_ms,
                msg: serde_json::from_str::<RuntimeEventMsg>(&msg_json)?,
            });
        }
        Ok(events)
    }

    fn list_runtime_events_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<RuntimeEvent>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, turn_id, sequence_no, msg_json, created_at_ms
             FROM protocol_runtime_events
             WHERE session_id = ?1
             ORDER BY rowid ASC",
        )?;
        let rows = statement.query_map(params![session_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, turn_id, sequence_no, msg_json, created_at_ms) = row?;
            events.push(RuntimeEvent {
                id: parse_protocol_id::<RuntimeEventId>(&id, "runtime event")?,
                session_id,
                turn_id: parse_protocol_id::<TurnId>(&turn_id, "runtime event turn")?,
                sequence_no,
                created_at_ms,
                msg: serde_json::from_str::<RuntimeEventMsg>(&msg_json)?,
            });
        }
        Ok(events)
    }

    fn list_history_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<HistoryItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
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
        let mut items = Vec::new();
        for row in rows {
            let (id, sequence_no, payload_json, created_at_ms) = row?;
            items.push(HistoryItem {
                id: parse_protocol_id::<HistoryItemId>(&id, "history item")?,
                session_id,
                turn_id,
                sequence_no,
                created_at_ms,
                payload: serde_json::from_str::<HistoryItemPayload>(&payload_json)?,
            });
        }
        Ok(items)
    }

    fn list_history_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<HistoryItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        list_history_items_for_session_from_connection(&connection, session_id)
    }

    fn collaboration_mode_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<ModeKind, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let items = list_history_items_for_session_from_connection(&connection, session_id)?;
        Ok(collaboration_mode_from_history(&items))
    }

    fn set_collaboration_mode(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        mode: ModeKind,
    ) -> Result<Option<HistoryItem>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let items = list_history_items_for_session_from_connection(&transaction, session_id)?;
        if collaboration_mode_from_history(&items) == mode {
            transaction.commit()?;
            return Ok(None);
        }
        let item = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: claim_protocol_sequence_in_transaction(
                &transaction,
                session_id,
                turn_id,
                0,
            )?,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::CollaborationModeInstruction { mode },
        };
        insert_history_item(&transaction, &item)?;
        transaction.commit()?;
        Ok(Some(item))
    }

    fn list_turn_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<TurnItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, source_item_id, sequence_no, payload_json
             FROM protocol_turn_items
             WHERE session_id = ?1 AND turn_id = ?2
             ORDER BY sequence_no ASC",
        )?;
        let rows = statement.query_map(
            params![session_id.to_string(), turn_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in rows {
            let (id, source_item_id, sequence_no, payload_json) = row?;
            items.push(TurnItem {
                id: parse_protocol_id::<TurnItemId>(&id, "turn item")?,
                session_id,
                turn_id,
                source_item_id: source_item_id
                    .map(|id| parse_protocol_id::<HistoryItemId>(&id, "turn item source"))
                    .transpose()?,
                sequence_no,
                payload: serde_json::from_str::<TurnItemPayload>(&payload_json)?,
            });
        }
        Ok(items)
    }

    fn list_turn_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TurnItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        list_turn_items_for_session_from_connection(&connection, session_id)
    }

    fn latest_turn_position_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(TurnId, i64)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        latest_turn_position_for_session(&*connection, session_id)
    }

    fn fork_canonical_items(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<(usize, usize), StorageError> {
        if source_session_id == target_session_id {
            return Err(StorageError::Message(
                "cannot fork canonical items into the same session".to_string(),
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let copied = fork_canonical_items_in_transaction(
            &transaction,
            source_session_id,
            target_session_id,
        )?;
        transaction.commit()?;
        Ok(copied)
    }

    fn fork_agent_context(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<usize, StorageError> {
        if source_session_id == target_session_id {
            return Err(StorageError::Message(
                "cannot fork agent context into the same session".to_string(),
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_empty_protocol_target(&transaction, target_session_id, "agent context")?;

        let source_history =
            list_history_items_for_session_from_connection(&transaction, source_session_id)?;
        let compacted_item_ids = compacted_history_item_ids(&source_history);
        let forked_history = source_history
            .into_iter()
            .filter(|item| !compacted_item_ids.contains(&item.id))
            .filter_map(|item| {
                fork_agent_context_payload(item.payload.clone()).map(|payload| HistoryItem {
                    id: HistoryItemId::new(),
                    session_id: target_session_id,
                    payload,
                    ..item
                })
            })
            .collect::<Vec<_>>();

        for item in &forked_history {
            insert_history_item(&transaction, item)?;
        }
        seed_history_turn_sequence_allocators(&transaction, target_session_id, &forked_history)?;
        transaction.commit()?;
        Ok(forked_history.len())
    }
}

pub(crate) fn fork_canonical_items_in_transaction(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
) -> Result<(usize, usize), StorageError> {
    if source_session_id == target_session_id {
        return Err(StorageError::Message(
            "cannot fork canonical items into the same session".to_string(),
        ));
    }
    ensure_empty_protocol_target(transaction, target_session_id, "canonical items")?;
    let source_history =
        list_history_items_for_session_from_connection(transaction, source_session_id)?;
    let source_turns = list_turn_items_for_session_from_connection(transaction, source_session_id)?;
    let history_id_map = source_history
        .iter()
        .map(|item| (item.id, HistoryItemId::new()))
        .collect::<HashMap<_, _>>();
    let mut forked_history = Vec::with_capacity(source_history.len());
    for item in source_history {
        let new_id = history_id_map[&item.id];
        forked_history.push(HistoryItem {
            id: new_id,
            session_id: target_session_id,
            payload: fork_history_payload_for_session(item.payload, &history_id_map)?,
            ..item
        });
    }
    let mut forked_turns = Vec::with_capacity(source_turns.len());
    for item in source_turns {
        let source_item_id = item
            .source_item_id
            .map(|source_id| {
                history_id_map.get(&source_id).copied().ok_or_else(|| {
                    StorageError::Message(format!(
                        "cannot fork turn item {}; source history item {} was not copied",
                        item.id, source_id
                    ))
                })
            })
            .transpose()?;
        forked_turns.push(TurnItem {
            id: TurnItemId::new(),
            session_id: target_session_id,
            source_item_id,
            ..item
        });
    }
    for item in &forked_history {
        insert_history_item(transaction, item)?;
    }
    for item in &forked_turns {
        insert_turn_item(transaction, item)?;
    }
    let mut next_sequence_by_turn = HashMap::<TurnId, i64>::new();
    for (turn_id, sequence_no) in forked_history
        .iter()
        .map(|item| (item.turn_id, item.sequence_no))
        .chain(
            forked_turns
                .iter()
                .map(|item| (item.turn_id, item.sequence_no)),
        )
    {
        let next_sequence_no = sequence_no.max(-1).saturating_add(1);
        next_sequence_by_turn
            .entry(turn_id)
            .and_modify(|current| *current = (*current).max(next_sequence_no))
            .or_insert(next_sequence_no);
    }
    for (turn_id, next_sequence_no) in next_sequence_by_turn {
        transaction.execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id, turn_id) DO UPDATE SET
                 next_sequence_no = MAX(
                     protocol_turn_sequence_allocators.next_sequence_no,
                     excluded.next_sequence_no
                 )",
            params![
                target_session_id.to_string(),
                turn_id.to_string(),
                next_sequence_no
            ],
        )?;
    }
    Ok((forked_history.len(), forked_turns.len()))
}

fn ensure_empty_protocol_target(
    transaction: &Transaction<'_>,
    target_session_id: SessionId,
    fork_label: &str,
) -> Result<(), StorageError> {
    let target_has_protocol_data = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM protocol_runtime_events WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_history_items WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_turn_items WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_item_append_order WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_turn_sequence_allocators WHERE session_id = ?1
         )",
        params![target_session_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if target_has_protocol_data {
        return Err(StorageError::Message(format!(
            "cannot fork {fork_label} into non-empty target session {target_session_id}"
        )));
    }
    Ok(())
}

fn fork_agent_context_payload(payload: HistoryItemPayload) -> Option<HistoryItemPayload> {
    match payload {
        HistoryItemPayload::UserTurn { content, .. } => Some(HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch: None,
            editor_context: None,
        }),
        HistoryItemPayload::AssistantMessage {
            response_id,
            content,
        } => Some(HistoryItemPayload::AssistantMessage {
            response_id,
            content,
        }),
        HistoryItemPayload::CollaborationModeInstruction { mode } => {
            Some(HistoryItemPayload::CollaborationModeInstruction { mode })
        }
        HistoryItemPayload::Compaction { mode, summary, .. } => {
            // The child receives the parent's current semantic summary, not the
            // replaced raw history. Empty replacement lineage makes the copied
            // summary a standalone active context item without inventing an
            // assistant response or retaining inactive parent details.
            Some(HistoryItemPayload::Compaction {
                mode,
                summary,
                replacement_item_ids: Vec::new(),
            })
        }
        _ => None,
    }
}

fn collaboration_mode_from_history(items: &[HistoryItem]) -> ModeKind {
    items
        .iter()
        .rev()
        .find_map(|item| match item.payload {
            HistoryItemPayload::CollaborationModeInstruction { mode } => Some(mode),
            _ => None,
        })
        .unwrap_or_default()
}

fn seed_history_turn_sequence_allocators(
    transaction: &Transaction<'_>,
    target_session_id: SessionId,
    history_items: &[HistoryItem],
) -> Result<(), StorageError> {
    let mut next_sequence_by_turn = HashMap::<TurnId, i64>::new();
    for item in history_items {
        let next_sequence_no = item.sequence_no.max(-1).saturating_add(1);
        next_sequence_by_turn
            .entry(item.turn_id)
            .and_modify(|current| *current = (*current).max(next_sequence_no))
            .or_insert(next_sequence_no);
    }
    for (turn_id, next_sequence_no) in next_sequence_by_turn {
        transaction.execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id, turn_id) DO UPDATE SET
                 next_sequence_no = MAX(
                     protocol_turn_sequence_allocators.next_sequence_no,
                     excluded.next_sequence_no
                 )",
            params![
                target_session_id.to_string(),
                turn_id.to_string(),
                next_sequence_no
            ],
        )?;
    }
    Ok(())
}

fn list_history_items_for_session_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Vec<HistoryItem>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT history.id, history.turn_id, history.sequence_no, history.payload_json, history.created_at_ms
         FROM protocol_history_items AS history
         LEFT JOIN protocol_item_append_order AS append_order
           ON append_order.source_kind = 'history_item'
          AND append_order.source_id = history.id
         WHERE history.session_id = ?1
         ORDER BY COALESCE(append_order.append_position, history.rowid) ASC",
    )?;
    let rows = statement.query_map(params![session_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut items = Vec::new();
    for row in rows {
        let (id, turn_id, sequence_no, payload_json, created_at_ms) = row?;
        items.push(HistoryItem {
            id: parse_protocol_id::<HistoryItemId>(&id, "history item")?,
            session_id,
            turn_id: parse_protocol_id::<TurnId>(&turn_id, "history item turn")?,
            sequence_no,
            created_at_ms,
            payload: serde_json::from_str::<HistoryItemPayload>(&payload_json)?,
        });
    }
    Ok(items)
}

fn list_turn_items_for_session_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Vec<TurnItem>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, turn_id, source_item_id, sequence_no, payload_json
         FROM protocol_turn_items
         WHERE session_id = ?1
         ORDER BY rowid ASC",
    )?;
    let rows = statement.query_map(params![session_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut items = Vec::new();
    for row in rows {
        let (id, turn_id, source_item_id, sequence_no, payload_json) = row?;
        items.push(TurnItem {
            id: parse_protocol_id::<TurnItemId>(&id, "turn item")?,
            session_id,
            turn_id: parse_protocol_id::<TurnId>(&turn_id, "turn item turn")?,
            source_item_id: source_item_id
                .map(|id| parse_protocol_id::<HistoryItemId>(&id, "turn item source"))
                .transpose()?,
            sequence_no,
            payload: serde_json::from_str::<TurnItemPayload>(&payload_json)?,
        });
    }
    Ok(items)
}

fn fork_history_payload_for_session(
    payload: HistoryItemPayload,
    history_id_map: &HashMap<HistoryItemId, HistoryItemId>,
) -> Result<HistoryItemPayload, StorageError> {
    match payload {
        HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch,
            editor_context,
        } => Ok(HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch,
            editor_context,
        }),
        HistoryItemPayload::Compaction {
            mode,
            summary,
            replacement_item_ids,
        } => {
            let replacement_item_ids = replacement_item_ids
                .into_iter()
                .map(|source_id| {
                    history_id_map.get(&source_id).copied().ok_or_else(|| {
                        StorageError::Message(format!(
                            "cannot fork compaction reference; source history item {source_id} was not copied"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(HistoryItemPayload::Compaction {
                mode,
                summary,
                replacement_item_ids,
            })
        }
        other => Ok(other),
    }
}

pub(crate) fn latest_protocol_turn_ids_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    limit: usize,
) -> Result<Vec<TurnId>, StorageError> {
    let mut statement = transaction.prepare(
        "SELECT turn_id
         FROM (
           SELECT turn_id, MAX(append_position) AS last_position
           FROM protocol_item_append_order
           WHERE session_id = ?1
           GROUP BY turn_id
         )
         ORDER BY last_position DESC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![session_id.to_string(), limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    let mut turn_ids = Vec::new();
    for row in rows {
        let value = row?;
        turn_ids.push(parse_protocol_id::<TurnId>(&value, "rollback turn")?);
    }
    Ok(turn_ids)
}

pub(crate) fn latest_turn_position_for_session(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Option<(TurnId, i64)>, StorageError> {
    let latest_turn_id = query_latest_protocol_turn_id(connection, session_id)?;
    let Some(latest_turn_id) = latest_turn_id else {
        return Ok(None);
    };
    let max_sequence_no = connection.query_row(
        "SELECT MAX(sequence_no)
         FROM (
           SELECT sequence_no FROM protocol_runtime_events WHERE session_id = ?1 AND turn_id = ?2
           UNION ALL
           SELECT sequence_no FROM protocol_history_items WHERE session_id = ?1 AND turn_id = ?2
           UNION ALL
           SELECT sequence_no FROM protocol_turn_items WHERE session_id = ?1 AND turn_id = ?2
         )",
        params![session_id.to_string(), latest_turn_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(Some((latest_turn_id, max_sequence_no.unwrap_or(-1) + 1)))
}

fn query_latest_protocol_turn_id(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Option<TurnId>, StorageError> {
    let value = connection.query_row(
        "SELECT turn_id
         FROM protocol_item_append_order
         WHERE session_id = ?1
         ORDER BY append_position DESC
         LIMIT 1",
        params![session_id.to_string()],
        |row| row.get::<_, String>(0),
    );
    match value {
        Ok(value) => Ok(Some(parse_protocol_id::<TurnId>(
            &value,
            "latest protocol turn",
        )?)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(StorageError::from(error)),
    }
}

trait ProtocolSqlExecutor {
    fn execute_protocol(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> rusqlite::Result<usize>;
}

impl ProtocolSqlExecutor for Connection {
    fn execute_protocol(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> rusqlite::Result<usize> {
        self.execute(sql, params)
    }
}

impl ProtocolSqlExecutor for Transaction<'_> {
    fn execute_protocol(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> rusqlite::Result<usize> {
        self.execute(sql, params)
    }
}

fn insert_runtime_event(
    connection: &impl ProtocolSqlExecutor,
    event: &RuntimeEvent,
) -> Result<(), StorageError> {
    let msg_json = serde_json::to_string(&event.msg)?;
    connection.execute_protocol(
        "INSERT INTO protocol_runtime_events (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            &event.id.to_string(),
            &event.session_id.to_string(),
            &event.turn_id.to_string(),
            &event.sequence_no,
            &msg_json,
            &hash_text(&msg_json),
            &event.created_at_ms,
        ],
    )?;
    insert_protocol_append_order(
        connection,
        event.session_id,
        event.turn_id,
        event.sequence_no,
        "runtime_event",
        &event.id.to_string(),
        event.created_at_ms,
    )?;
    Ok(())
}

pub(crate) fn insert_session_owned_event_bundle_in_transaction(
    _authority: &SessionProtocolWriteAuthority,
    transaction: &Transaction<'_>,
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<StoredProtocolEventBundle, StorageError> {
    insert_event_bundle_unchecked(transaction, event, history_item, turn_item)
}

fn insert_event_bundle_unchecked(
    transaction: &Transaction<'_>,
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<StoredProtocolEventBundle, StorageError> {
    validate_event_bundle_coherence(event, history_item, turn_item)?;
    let sequence_no = claim_protocol_sequence_in_transaction(
        transaction,
        event.session_id,
        event.turn_id,
        event.sequence_no,
    )?;
    let mut runtime_event = event.clone();
    runtime_event.sequence_no = sequence_no;
    let history_item = history_item.map(|item| {
        let mut item = item.clone();
        item.sequence_no = sequence_no;
        item
    });
    let turn_item = turn_item.map(|item| {
        let mut item = item.clone();
        item.sequence_no = sequence_no;
        item
    });
    insert_runtime_event(transaction, &runtime_event)?;
    if let Some(history_item) = &history_item {
        insert_history_item(transaction, history_item)?;
    }
    if let Some(turn_item) = &turn_item {
        insert_turn_item(transaction, turn_item)?;
    }
    Ok(StoredProtocolEventBundle {
        runtime_event,
        history_item,
    })
}

fn validate_recording_projection(
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<(), StorageError> {
    validate_event_bundle_coherence(event, history_item, turn_item)?;
    let history_payload = history_item.map(|item| &item.payload);
    let turn_payload = turn_item.map(|item| &item.payload);
    let allowed = match (&event.msg, history_payload, turn_payload) {
        // Session lifecycle notices are runtime-only. They do not own canonical
        // user/model/tool state.
        (RuntimeEventMsg::Warning { .. }, None, None) => true,
        (
            RuntimeEventMsg::Warning { message },
            Some(HistoryItemPayload::Error {
                message: history_message,
            }),
            Some(TurnItemPayload::Error {
                message: turn_message,
            }),
        ) => message == history_message && message == turn_message,
        (
            RuntimeEventMsg::ModelRequestPrepared { .. },
            Some(HistoryItemPayload::RequestDiagnostics { .. }),
            None,
        ) => true,
        (
            RuntimeEventMsg::WorldStateUpdated { .. },
            Some(HistoryItemPayload::WorldState { .. }),
            Some(TurnItemPayload::WorldState { .. }),
        ) => true,
        (
            RuntimeEventMsg::ApprovalRequested { call_id, .. },
            None,
            Some(TurnItemPayload::ApprovalRequest {
                call_id: turn_call_id,
                ..
            }),
        ) => call_id == turn_call_id,
        (
            RuntimeEventMsg::ApprovalResolved { call_id, .. },
            Some(HistoryItemPayload::ApprovalDecision {
                call_id: history_call_id,
                ..
            }),
            None,
        ) => call_id == history_call_id,
        (
            RuntimeEventMsg::ContextCompacted { item_id, mode },
            Some(HistoryItemPayload::Compaction {
                mode: history_mode, ..
            }),
            Some(TurnItemPayload::ContextCompaction { .. }),
        ) => history_item.is_some_and(|item| item.id == *item_id) && mode == history_mode,
        (
            RuntimeEventMsg::RetryScheduled {
                attempt,
                message,
                next_retry_at_ms,
            },
            Some(HistoryItemPayload::RetryDecision {
                attempt: history_attempt,
                message: history_message,
                next_retry_at_ms: history_retry_at_ms,
            }),
            Some(TurnItemPayload::Warning {
                message: turn_message,
            }),
        ) => {
            attempt == history_attempt
                && message == history_message
                && message == turn_message
                && next_retry_at_ms == history_retry_at_ms
        }
        _ => false,
    };
    if allowed {
        return Ok(());
    }
    Err(StorageError::Message(format!(
        "protocol recording sink cannot own runtime projection `{}`; use its atomic state owner",
        runtime_event_kind(&event.msg)
    )))
}

fn runtime_event_kind(message: &RuntimeEventMsg) -> &'static str {
    match message {
        RuntimeEventMsg::ThreadConfigured { .. } => "thread_configured",
        RuntimeEventMsg::UserInputAccepted { .. } => "user_input_accepted",
        RuntimeEventMsg::SteerInputAccepted { .. } => "steer_input_accepted",
        RuntimeEventMsg::InterAgentCommunicationReceived { .. } => {
            "inter_agent_communication_received"
        }
        RuntimeEventMsg::SubAgentActivity { .. } => "sub_agent_activity",
        RuntimeEventMsg::AssistantTextDelta { .. } => "assistant_text_delta",
        RuntimeEventMsg::AssistantMessageCommitted { .. } => "assistant_message_committed",
        RuntimeEventMsg::ReasoningSummaryDelta { .. } => "reasoning_summary_delta",
        RuntimeEventMsg::ModelRequestPrepared { .. } => "model_request_prepared",
        RuntimeEventMsg::WorldStateUpdated { .. } => "world_state_updated",
        RuntimeEventMsg::HistoryItemRecorded { .. } => "history_item_recorded",
        RuntimeEventMsg::ToolLifecycle { .. } => "tool_lifecycle",
        RuntimeEventMsg::ApprovalRequested { .. } => "approval_requested",
        RuntimeEventMsg::ApprovalResolved { .. } => "approval_resolved",
        RuntimeEventMsg::ContextCompacted { .. } => "context_compacted",
        RuntimeEventMsg::FileChangesRecorded { .. } => "file_changes_recorded",
        RuntimeEventMsg::Warning { .. } => "warning",
        RuntimeEventMsg::RetryScheduled { .. } => "retry_scheduled",
        RuntimeEventMsg::TurnTerminal { .. } => "turn_terminal",
    }
}

fn claim_protocol_sequence_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    requested_sequence_no: i64,
) -> Result<i64, StorageError> {
    let stored_max = transaction.query_row(
        "SELECT MAX(sequence_no)
         FROM (
             SELECT sequence_no FROM protocol_runtime_events WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL
             SELECT sequence_no FROM protocol_history_items WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL
             SELECT sequence_no FROM protocol_turn_items WHERE session_id = ?1 AND turn_id = ?2
         )",
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    let sequence_floor = requested_sequence_no
        .max(0)
        .max(stored_max.unwrap_or(-1).saturating_add(1));
    let claimed = transaction.query_row(
        "INSERT INTO protocol_turn_sequence_allocators
             (session_id, turn_id, next_sequence_no)
         VALUES (?1, ?2, ?3 + 1)
         ON CONFLICT(session_id, turn_id) DO UPDATE SET
             next_sequence_no = MAX(
                 protocol_turn_sequence_allocators.next_sequence_no,
                 excluded.next_sequence_no - 1
             ) + 1
         RETURNING next_sequence_no - 1",
        params![session_id.to_string(), turn_id.to_string(), sequence_floor],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(claimed)
}

fn validate_event_bundle_coherence(
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<(), StorageError> {
    if let Some(history_item) = history_item {
        validate_event_history_identity(event, history_item)?;
    }
    if let Some(turn_item) = turn_item {
        validate_event_turn_identity(event, turn_item)?;
    }
    if let (Some(history_item), Some(turn_item)) = (history_item, turn_item) {
        validate_history_turn_bundle_coherence(history_item, turn_item)?;
    }
    Ok(())
}

fn validate_event_history_identity(
    event: &RuntimeEvent,
    history_item: &HistoryItem,
) -> Result<(), StorageError> {
    if event.session_id != history_item.session_id {
        return Err(StorageError::Message(format!(
            "protocol event bundle session mismatch: event session `{}` history item session `{}`",
            event.session_id, history_item.session_id
        )));
    }
    if event.turn_id != history_item.turn_id {
        return Err(StorageError::Message(format!(
            "protocol event bundle turn mismatch: event turn `{}` history item turn `{}`",
            event.turn_id, history_item.turn_id
        )));
    }
    Ok(())
}

fn validate_event_turn_identity(
    event: &RuntimeEvent,
    turn_item: &TurnItem,
) -> Result<(), StorageError> {
    if event.session_id != turn_item.session_id {
        return Err(StorageError::Message(format!(
            "protocol event bundle session mismatch: event session `{}` turn item session `{}`",
            event.session_id, turn_item.session_id
        )));
    }
    if event.turn_id != turn_item.turn_id {
        return Err(StorageError::Message(format!(
            "protocol event bundle turn mismatch: event turn `{}` turn item turn `{}`",
            event.turn_id, turn_item.turn_id
        )));
    }
    Ok(())
}

fn validate_history_turn_bundle_coherence(
    history_item: &HistoryItem,
    turn_item: &TurnItem,
) -> Result<(), StorageError> {
    if history_item.session_id != turn_item.session_id {
        return Err(StorageError::Message(format!(
            "protocol history-turn bundle session mismatch: history item session `{}` turn item session `{}`",
            history_item.session_id, turn_item.session_id
        )));
    }
    if history_item.turn_id != turn_item.turn_id {
        return Err(StorageError::Message(format!(
            "protocol history-turn bundle turn mismatch: history item turn `{}` turn item turn `{}`",
            history_item.turn_id, turn_item.turn_id
        )));
    }
    if turn_item.source_item_id != Some(history_item.id) {
        return Err(StorageError::Message(format!(
            "protocol history-turn bundle source mismatch: turn item source `{:?}` history item id `{}`",
            turn_item.source_item_id, history_item.id
        )));
    }
    Ok(())
}

fn insert_history_item(
    connection: &impl ProtocolSqlExecutor,
    item: &HistoryItem,
) -> Result<(), StorageError> {
    let payload_json = serde_json::to_string(&item.payload)?;
    connection.execute_protocol(
        "INSERT INTO protocol_history_items (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            &item.id.to_string(),
            &item.session_id.to_string(),
            &item.turn_id.to_string(),
            &item.sequence_no,
            &payload_json,
            &hash_text(&payload_json),
            &item.created_at_ms,
        ],
    )?;
    insert_protocol_append_order(
        connection,
        item.session_id,
        item.turn_id,
        item.sequence_no,
        "history_item",
        &item.id.to_string(),
        item.created_at_ms,
    )?;
    Ok(())
}

fn insert_turn_item(
    connection: &impl ProtocolSqlExecutor,
    item: &TurnItem,
) -> Result<(), StorageError> {
    let payload_json = serde_json::to_string(&item.payload)?;
    let source_item_id = item.source_item_id.map(|id| id.to_string());
    connection.execute_protocol(
        "INSERT INTO protocol_turn_items (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            &item.id.to_string(),
            &item.session_id.to_string(),
            &item.turn_id.to_string(),
            &source_item_id,
            &item.sequence_no,
            &payload_json,
            &hash_text(&payload_json),
        ],
    )?;
    insert_protocol_append_order(
        connection,
        item.session_id,
        item.turn_id,
        item.sequence_no,
        "turn_item",
        &item.id.to_string(),
        0,
    )?;
    Ok(())
}

fn insert_protocol_append_order(
    connection: &impl ProtocolSqlExecutor,
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
    source_kind: &str,
    source_id: &str,
    created_at_ms: i64,
) -> Result<(), StorageError> {
    connection.execute_protocol(
        "INSERT OR IGNORE INTO protocol_item_append_order
            (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        &[
            &session_id.to_string(),
            &turn_id.to_string(),
            &sequence_no,
            &source_kind,
            &source_id,
            &created_at_ms,
        ],
    )?;
    Ok(())
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn parse_protocol_id<T>(value: &str, label: &str) -> Result<T, StorageError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value.parse::<T>().map_err(|error| {
        StorageError::Message(format!("invalid protocol {label} id `{value}`: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::Duration;

    use rusqlite::Connection;

    use super::*;
    use crate::protocol::ContentPart;

    #[test]
    fn collaboration_mode_is_history_owned_noop_safe_and_resume_replayable() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(Arc::clone(&connection));
        let session_id = SessionId::new();

        assert_eq!(
            store
                .collaboration_mode_for_session(session_id)
                .expect("initial mode"),
            ModeKind::Default
        );
        assert!(
            store
                .set_collaboration_mode(session_id, TurnId::new(), ModeKind::Default)
                .expect("same default")
                .is_none(),
            "the protocol default is already effective and must not create state"
        );

        let plan_turn = TurnId::new();
        let plan_item = store
            .set_collaboration_mode(session_id, plan_turn, ModeKind::Plan)
            .expect("set plan")
            .expect("new plan instruction");
        assert!(matches!(
            plan_item.payload,
            HistoryItemPayload::CollaborationModeInstruction {
                mode: ModeKind::Plan
            }
        ));
        assert!(
            store
                .set_collaboration_mode(session_id, TurnId::new(), ModeKind::Plan)
                .expect("same plan")
                .is_none(),
            "same-value updates must not append another instruction"
        );

        let resumed = SqliteProtocolEventStore::new(connection);
        assert_eq!(
            resumed
                .collaboration_mode_for_session(session_id)
                .expect("replayed mode"),
            ModeKind::Plan,
            "a new runtime owner must rehydrate mode solely from durable history"
        );
        assert_eq!(
            resumed
                .list_history_items_for_session(session_id)
                .expect("mode history")
                .iter()
                .filter(|item| matches!(
                    &item.payload,
                    HistoryItemPayload::CollaborationModeInstruction { .. }
                ))
                .count(),
            1
        );

        let default_turn = TurnId::new();
        resumed
            .set_collaboration_mode(session_id, default_turn, ModeKind::Default)
            .expect("restore default")
            .expect("default instruction");
        assert_eq!(
            resumed
                .collaboration_mode_for_session(session_id)
                .expect("restored mode"),
            ModeKind::Default
        );
    }

    #[test]
    fn list_history_items_for_session_uses_append_order_across_turns() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let older = history_user_turn(session_id, TurnId::new(), 29, 100, "older-stage");
        let newer = history_user_turn(session_id, TurnId::new(), 15, 200, "newer-stage");

        store
            .seed_history_item_for_test(&older)
            .expect("older insert");
        store
            .seed_history_item_for_test(&newer)
            .expect("newer insert");

        let listed = store
            .list_history_items_for_session(session_id)
            .expect("history list");
        assert_eq!(
            listed.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![older.id, newer.id]
        );
    }

    #[test]
    fn fork_remaps_compaction_replacement_history_ids() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        let turn_id = TurnId::new();
        let replaced = history_user_turn(source_session_id, turn_id, 0, 100, "old detail");
        let compaction = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 200,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "old detail summary".to_string(),
                replacement_item_ids: vec![replaced.id],
            },
        };
        store
            .seed_history_item_for_test(&replaced)
            .expect("replaced item");
        store
            .seed_history_item_for_test(&compaction)
            .expect("compaction item");

        let copied = store
            .fork_canonical_items(source_session_id, target_session_id)
            .expect("fork canonical items");
        let forked = store
            .list_history_items_for_session(target_session_id)
            .expect("forked history");

        assert_eq!(copied, (2, 0));
        assert_ne!(forked[0].id, replaced.id);
        let HistoryItemPayload::Compaction {
            replacement_item_ids,
            ..
        } = &forked[1].payload
        else {
            panic!("second forked item should be compaction");
        };
        assert_eq!(replacement_item_ids.as_slice(), &[forked[0].id]);
        assert!(!replacement_item_ids.contains(&replaced.id));
    }

    #[test]
    fn fork_agent_context_copies_only_model_visible_parent_messages() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        let turn_id = TurnId::new();
        let user_turn = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: 10,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: "investigate the protocol".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        };
        let assistant = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 30,
            payload: HistoryItemPayload::AssistantMessage {
                response_id: crate::protocol::ModelResponseId::new(),
                content: vec![ContentPart::Text {
                    text: "parent result".to_string(),
                }],
            },
        };
        let communication = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 40,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: crate::protocol::InterAgentCommunication {
                    author: "/root/reviewer".to_string(),
                    recipient: "/root".to_string(),
                    content: "review feedback".to_string(),
                    trigger_turn: false,
                },
            },
        };
        for item in [&user_turn, &assistant, &communication] {
            store
                .seed_history_item_for_test(item)
                .expect("source append");
        }

        let copied = store
            .fork_agent_context(source_session_id, target_session_id)
            .expect("agent context fork");
        let forked = store
            .list_history_items_for_session(target_session_id)
            .expect("forked history");

        assert_eq!(copied, 2);
        assert_eq!(forked.len(), 2);
        assert_eq!(forked[0].session_id, target_session_id);
        assert_eq!(forked[1].session_id, target_session_id);
        assert_ne!(forked[0].id, user_turn.id);
        assert_ne!(forked[1].id, assistant.id);
        assert!(matches!(
            &forked[0].payload,
            HistoryItemPayload::UserTurn {
                content,
                prompt_dispatch: None,
                editor_context: None,
            } if matches!(content.as_slice(), [ContentPart::Text { text }] if text == "investigate the protocol")
        ));
        assert!(matches!(
            &forked[1].payload,
            HistoryItemPayload::AssistantMessage {
                content,
                ..
            } if matches!(content.as_slice(), [ContentPart::Text { text }] if text == "parent result")
        ));
        assert!(
            store
                .list_turn_items_for_session(target_session_id)
                .expect("forked turn items")
                .is_empty()
        );

        store
            .seed_runtime_event_for_test(&warning_event(
                target_session_id,
                turn_id,
                0,
                "after fork",
            ))
            .expect("post-fork append");
        assert_eq!(
            store
                .list_runtime_events(target_session_id, turn_id)
                .expect("events")[0]
                .sequence_no,
            3
        );
    }

    #[test]
    fn fork_agent_context_preserves_compacted_view_without_resurrecting_replaced_items() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        let turn_id = TurnId::new();
        let old_user = history_user_turn(source_session_id, turn_id, 0, 10, "obsolete detail");
        let old_assistant = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 20,
            payload: HistoryItemPayload::AssistantMessage {
                response_id: crate::protocol::ModelResponseId::new(),
                content: vec![ContentPart::Text {
                    text: "obsolete response".to_string(),
                }],
            },
        };
        let compaction = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 30,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "the earlier exchange established the compacted contract".to_string(),
                replacement_item_ids: vec![old_user.id, old_assistant.id],
            },
        };
        let current_user = history_user_turn(
            source_session_id,
            turn_id,
            3,
            40,
            "continue from the compacted contract",
        );
        for item in [&old_user, &old_assistant, &compaction, &current_user] {
            store
                .seed_history_item_for_test(item)
                .expect("source append");
        }

        assert_eq!(
            store
                .fork_agent_context(source_session_id, target_session_id)
                .expect("agent context fork"),
            2
        );
        let forked = store
            .list_history_items_for_session(target_session_id)
            .expect("forked history");

        assert_eq!(forked.len(), 2);
        assert!(matches!(
            &forked[0].payload,
            HistoryItemPayload::Compaction {
                summary,
                replacement_item_ids,
                ..
            } if summary.contains("compacted contract") && replacement_item_ids.is_empty()
        ));
        assert!(matches!(
            &forked[1].payload,
            HistoryItemPayload::UserTurn { content, .. }
                if matches!(content.as_slice(), [ContentPart::Text { text }]
                    if text == "continue from the compacted contract")
        ));
        let messages =
            crate::agent::context_manager::ContextManager::rehydrate(forked).model_messages(false);
        assert!(matches!(
            messages.as_slice(),
            [
                crate::llm::ModelMessage::System { content: summary },
                crate::llm::ModelMessage::User { content: current }
            ] if summary.contains("compacted contract")
                && current == "continue from the compacted contract"
        ));
    }

    #[test]
    fn fork_agent_context_rejects_non_empty_target() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        let turn_id = TurnId::new();
        store
            .seed_history_item_for_test(&history_user_turn(
                source_session_id,
                turn_id,
                0,
                1,
                "source",
            ))
            .expect("source append");
        store
            .seed_runtime_event_for_test(&warning_event(
                target_session_id,
                TurnId::new(),
                0,
                "target",
            ))
            .expect("target append");

        let error = store
            .fork_agent_context(source_session_id, target_session_id)
            .expect_err("non-empty target must be rejected");
        assert!(error.to_string().contains("non-empty target session"));
    }

    #[test]
    fn concurrent_event_appends_claim_distinct_database_sequences() {
        let temp = tempfile::tempdir().expect("tempdir");
        let database_path = temp.path().join("protocol.sqlite3");
        let open_store = |path: &std::path::Path| {
            let connection = Connection::open(path).expect("database");
            connection
                .busy_timeout(Duration::from_secs(5))
                .expect("busy timeout");
            crate::storage::migration::run(&connection).expect("migrations");
            SqliteProtocolEventStore::new(Arc::new(Mutex::new(connection)))
        };
        let first_store = open_store(&database_path);
        let second_store = open_store(&database_path);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let first_event = warning_event(session_id, turn_id, 0, "first");
        let second_event = warning_event(session_id, turn_id, 0, "second");
        let expected_ids = [first_event.id, second_event.id];
        let barrier = Arc::new(Barrier::new(3));

        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_store
                .append_recording_projection_allocating(&first_event, None, None)
                .expect("first append")
                .runtime_event
        });
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_store
                .append_recording_projection_allocating(&second_event, None, None)
                .expect("second append")
                .runtime_event
        });
        barrier.wait();
        let stored = [
            first.join().expect("first worker"),
            second.join().expect("second worker"),
        ];
        let mut sequence_numbers = stored
            .iter()
            .map(|event| event.sequence_no)
            .collect::<Vec<_>>();
        sequence_numbers.sort_unstable();
        assert_eq!(sequence_numbers, vec![0, 1]);
        assert!(
            expected_ids
                .iter()
                .all(|expected| stored.iter().any(|event| event.id == *expected))
        );

        let final_store = open_store(&database_path);
        let persisted = final_store
            .list_runtime_events(session_id, turn_id)
            .expect("persisted events");
        assert_eq!(
            persisted
                .iter()
                .map(|event| event.sequence_no)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            expected_ids
                .iter()
                .all(|expected| persisted.iter().any(|event| event.id == *expected))
        );
    }

    #[test]
    fn test_seed_helpers_share_one_turn_allocator() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let history = history_user_turn(session_id, turn_id, 0, 1, "history");
        let turn = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no: 0,
            payload: TurnItemPayload::AgentMessage {
                text: "turn".to_string(),
            },
        };
        let event = warning_event(session_id, turn_id, 0, "runtime");

        store
            .seed_history_item_for_test(&history)
            .expect("history append");
        store.seed_turn_item_for_test(&turn).expect("turn append");
        store
            .seed_runtime_event_for_test(&event)
            .expect("event append");

        assert_eq!(
            store
                .list_history_items(session_id, turn_id)
                .expect("history")[0]
                .sequence_no,
            0
        );
        assert_eq!(
            store.list_turn_items(session_id, turn_id).expect("turns")[0].sequence_no,
            1
        );
        assert_eq!(
            store
                .list_runtime_events(session_id, turn_id)
                .expect("events")[0]
                .sequence_no,
            2
        );
    }

    #[test]
    fn recording_projection_rejects_atomic_owner_payloads_without_writing_any_stream() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = crate::protocol::ModelResponseId::new();
        let tool_call_id = crate::session::ToolCallId::new();
        let forbidden = vec![
            crate::session::RunEvent::UserTurnStored {
                session_id,
                turn: Box::new(crate::protocol::UserTurn {
                    turn_id,
                    items: vec![crate::protocol::UserInputItem::Text {
                        text: "must use the user-turn owner".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                }),
            },
            crate::session::RunEvent::AssistantMessageCommitted {
                response_id,
                text: "must use the model-response owner".to_string(),
            },
            crate::session::RunEvent::ToolCallPending {
                tool_call_id,
                response_id,
                model_call_id: "provider-call".to_string(),
                tool_name: "read".to_string(),
                arguments_json: "{}".to_string(),
            },
            crate::session::RunEvent::ToolCallCompleted {
                tool_call_id,
                tool: crate::tool::ToolName::Read,
                title: "read".to_string(),
                summary: "must use the tool-settlement owner".to_string(),
                metadata: serde_json::Value::Null,
            },
            crate::session::RunEvent::FileChangesRecorded {
                tool_call_id,
                changes: Vec::new(),
            },
            crate::session::RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    status: crate::protocol::TurnTerminalStatus::Completed,
                    finish_reason: Some(crate::session::FinishReason::Stop),
                    interruption_cause: None,
                    final_response_id: Some(response_id),
                    summary: "must use the terminal owner".to_string(),
                    tool_call_count: 1,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                }),
            },
        ];

        for event in forbidden {
            let projection =
                crate::protocol::project_protocol_run_event(&event, Some(session_id), turn_id, 0)
                    .expect("forbidden event still has a projection for its atomic owner");
            let error = store
                .append_recording_projection_allocating(
                    &projection.runtime_event,
                    projection.history_item.as_ref(),
                    projection.turn_item.as_ref(),
                )
                .expect_err("recording projection must reject atomic-owner payload");
            assert!(error.to_string().contains("atomic state owner"));
        }

        assert!(
            store
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events")
                .is_empty()
        );
        assert!(
            store
                .list_history_items(session_id, turn_id)
                .expect("history items")
                .is_empty()
        );
        assert!(
            store
                .list_turn_items(session_id, turn_id)
                .expect("turn items")
                .is_empty()
        );
    }

    #[test]
    fn admitted_recording_requires_the_exact_active_turn_owner() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
            let project_id = crate::session::ProjectId::new();
            let session_id = SessionId::new();
            locked
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES (?1, 'C:/fixture', 'fixture', 'none', 1, 1)",
                    params![project_id.to_string()],
                )
                .expect("project fixture");
            locked
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      access_mode, model_parameters_json, created_at_ms, updated_at_ms,
                      completed_at_ms, active_run_id, active_turn_id,
                      active_run_lease_expires_at_ms)
                     VALUES (?1, ?2, 'fixture', 'running', 'C:/fixture', 'model',
                             'http://localhost', 'default', '{}', 1, 1, NULL,
                             'admission-without-turn', NULL, 1000)",
                    params![session_id.to_string(), project_id.to_string()],
                )
                .expect("legacy null-turn admission fixture");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = {
            let connection = store.connection.lock().expect("sqlite mutex");
            connection
                .query_row("SELECT id FROM sessions", [], |row| row.get::<_, String>(0))
                .expect("session id")
                .parse::<SessionId>()
                .expect("valid session id")
        };
        let turn_id = TurnId::new();
        let event = warning_event(session_id, turn_id, 0, "must have an exact turn owner");

        assert!(
            store
                .append_admitted_recording_projection_allocating_at(
                    "admission-without-turn",
                    &event,
                    None,
                    None,
                    100,
                )
                .expect("ownership query")
                .is_none(),
            "a legacy null-turn admission must not own a current protocol turn"
        );
        assert!(
            store
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events")
                .is_empty()
        );
    }

    #[test]
    fn sub_agent_activity_is_limited_to_the_transactionally_active_turn() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
            let project_id = crate::session::ProjectId::new();
            locked
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES (?1, 'C:/activity-fixture', 'fixture', 'none', 1, 1)",
                    params![project_id.to_string()],
                )
                .expect("project fixture");
            locked
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      access_mode, model_parameters_json, created_at_ms, updated_at_ms,
                      completed_at_ms, active_run_id, active_turn_id,
                      active_run_lease_expires_at_ms)
                     VALUES (?1, ?2, 'fixture', 'running', 'C:/activity-fixture', 'model',
                             'http://localhost', 'default', '{}', 1, 1, NULL,
                             'active-run', ?3, ?4)",
                    params![
                        session_id.to_string(),
                        project_id.to_string(),
                        turn_id.to_string(),
                        i64::MAX
                    ],
                )
                .expect("active turn fixture");
        }
        let store = SqliteProtocolEventStore::new(Arc::clone(&connection));
        store
            .append_sub_agent_activity(
                session_id,
                "activity-1".to_string(),
                SessionId::new(),
                "/root/reviewer".to_string(),
                SubAgentActivityKind::Interacted,
            )
            .expect("sub-agent activity append");

        assert!(matches!(
            store
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events")
                .as_slice(),
            [RuntimeEvent {
                sequence_no: 0,
                msg: RuntimeEventMsg::SubAgentActivity { .. },
                ..
            }]
        ));
        assert!(matches!(
            store
                .list_history_items(session_id, turn_id)
                .expect("history items")[0]
                .payload,
            HistoryItemPayload::SubAgentActivity { .. }
        ));
        assert!(matches!(
            store
                .list_turn_items(session_id, turn_id)
                .expect("turn items")[0]
                .payload,
            TurnItemPayload::SubAgentActivity { .. }
        ));

        connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "UPDATE sessions
                 SET status = 'completed', completed_at_ms = 2,
                     active_run_id = NULL, active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1",
                params![session_id.to_string()],
            )
            .expect("terminalize fixture");
        let error = store
            .append_sub_agent_activity(
                session_id,
                "activity-after-terminal".to_string(),
                SessionId::new(),
                "/root/reviewer".to_string(),
                SubAgentActivityKind::Interrupted,
            )
            .expect_err("terminal turn must reject later activity");
        assert!(error.to_string().contains("no active turn"));
        assert_eq!(
            store
                .list_history_items(session_id, turn_id)
                .expect("history after terminal race")
                .len(),
            1
        );
    }

    #[test]
    fn fork_seeds_allocator_after_copied_maximum_sequence() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        let turn_id = TurnId::new();
        store
            .seed_history_item_for_test(&history_user_turn(
                source_session_id,
                turn_id,
                29,
                1,
                "source",
            ))
            .expect("source append");
        store
            .fork_canonical_items(source_session_id, target_session_id)
            .expect("fork");
        store
            .seed_runtime_event_for_test(&warning_event(
                target_session_id,
                turn_id,
                0,
                "after fork",
            ))
            .expect("post-fork append");

        assert_eq!(
            store
                .list_runtime_events(target_session_id, turn_id)
                .expect("events")[0]
                .sequence_no,
            30
        );
    }

    fn warning_event(
        session_id: SessionId,
        turn_id: TurnId,
        sequence_no: i64,
        message: &str,
    ) -> RuntimeEvent {
        RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: 1,
            msg: RuntimeEventMsg::Warning {
                message: message.to_string(),
            },
        }
    }

    fn history_user_turn(
        session_id: SessionId,
        turn_id: TurnId,
        sequence_no: i64,
        created_at_ms: i64,
        text: &str,
    ) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: text.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        }
    }
}
