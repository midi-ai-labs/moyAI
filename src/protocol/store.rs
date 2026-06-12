use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, Transaction, params};
use sha2::{Digest, Sha256};

use crate::error::StorageError;
use crate::protocol::{
    HistoryItem, HistoryItemId, HistoryItemPayload, RuntimeEvent, RuntimeEventId, RuntimeEventMsg,
    TurnId, TurnItem, TurnItemId, TurnItemPayload,
};
use crate::session::SessionId;

pub trait ProtocolEventStore {
    fn append_runtime_event(&self, event: &RuntimeEvent) -> Result<(), StorageError>;
    fn append_event_bundle(
        &self,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<(), StorageError>;
    fn list_runtime_events(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<RuntimeEvent>, StorageError>;
    fn list_runtime_events_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<RuntimeEvent>, StorageError>;
    fn append_history_item(&self, item: &HistoryItem) -> Result<(), StorageError>;
    fn append_history_turn_bundle(
        &self,
        history_item: &HistoryItem,
        turn_item: &TurnItem,
    ) -> Result<(), StorageError>;
    fn list_history_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<HistoryItem>, StorageError>;
    fn list_history_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<HistoryItem>, StorageError>;
    fn append_turn_item(&self, item: &TurnItem) -> Result<(), StorageError>;
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
    fn rollback_latest_turns(
        &self,
        session_id: SessionId,
        num_turns: usize,
    ) -> Result<Vec<TurnId>, StorageError>;
    fn fork_canonical_items(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<(usize, usize), StorageError>;
}

#[derive(Clone)]
pub struct SqliteProtocolEventStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteProtocolEventStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

impl ProtocolEventStore for SqliteProtocolEventStore {
    fn append_runtime_event(&self, event: &RuntimeEvent) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        insert_runtime_event(&transaction, event)?;
        transaction.commit()?;
        Ok(())
    }

    fn append_event_bundle(
        &self,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<(), StorageError> {
        validate_event_bundle_coherence(event, history_item, turn_item)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        insert_runtime_event(&transaction, event)?;
        if let Some(history_item) = history_item {
            insert_history_item(&transaction, history_item)?;
        }
        if let Some(turn_item) = turn_item {
            insert_turn_item(&transaction, turn_item)?;
        }
        transaction.commit()?;
        Ok(())
    }

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

    fn append_history_item(&self, item: &HistoryItem) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        insert_history_item(&transaction, item)?;
        transaction.commit()?;
        Ok(())
    }

    fn append_history_turn_bundle(
        &self,
        history_item: &HistoryItem,
        turn_item: &TurnItem,
    ) -> Result<(), StorageError> {
        validate_history_turn_bundle_coherence(history_item, turn_item)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        insert_history_item(&transaction, history_item)?;
        insert_turn_item(&transaction, turn_item)?;
        transaction.commit()?;
        Ok(())
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
        let mut statement = connection.prepare(
            "SELECT id, turn_id, sequence_no, payload_json, created_at_ms
             FROM protocol_history_items
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

    fn append_turn_item(&self, item: &TurnItem) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        insert_turn_item(&transaction, item)?;
        transaction.commit()?;
        Ok(())
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

    fn latest_turn_position_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(TurnId, i64)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        latest_turn_position_for_session(&*connection, session_id)
    }

    fn rollback_latest_turns(
        &self,
        session_id: SessionId,
        num_turns: usize,
    ) -> Result<Vec<TurnId>, StorageError> {
        if num_turns == 0 {
            return Err(StorageError::Message(
                "rollback turn count must be greater than zero".to_string(),
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let turn_ids =
            latest_protocol_turn_ids_in_transaction(&transaction, session_id, num_turns)?;
        if turn_ids.len() < num_turns {
            return Err(StorageError::Message(format!(
                "cannot rollback {num_turns} turn(s); session {session_id} only has {} canonical turn(s)",
                turn_ids.len()
            )));
        }
        for turn_id in &turn_ids {
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
        }
        transaction.commit()?;
        Ok(turn_ids)
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
        let source_history = self.list_history_items_for_session(source_session_id)?;
        let source_turns = self.list_turn_items_for_session(source_session_id)?;
        let mut history_id_map = HashMap::new();
        let mut forked_history = Vec::with_capacity(source_history.len());
        for item in source_history {
            let new_id = HistoryItemId::new();
            history_id_map.insert(item.id, new_id);
            forked_history.push(HistoryItem {
                id: new_id,
                session_id: target_session_id,
                payload: fork_history_payload_for_session(item.payload, target_session_id),
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
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        for item in &forked_history {
            insert_history_item(&transaction, item)?;
        }
        for item in &forked_turns {
            insert_turn_item(&transaction, item)?;
        }
        transaction.commit()?;
        Ok((forked_history.len(), forked_turns.len()))
    }
}

fn fork_history_payload_for_session(
    payload: HistoryItemPayload,
    target_session_id: SessionId,
) -> HistoryItemPayload {
    match payload {
        HistoryItemPayload::UserTurn {
            message_id,
            content,
            prompt_dispatch,
            editor_context,
            turn_context,
        } => HistoryItemPayload::UserTurn {
            message_id,
            content,
            prompt_dispatch,
            editor_context,
            turn_context: turn_context.map(|mut context| {
                context.session_id = target_session_id;
                Box::new(*context)
            }),
        },
        HistoryItemPayload::ControlEnvelope { mut envelope } => {
            envelope.session_id = target_session_id;
            envelope.context.session_id = target_session_id;
            HistoryItemPayload::ControlEnvelope { envelope }
        }
        other => other,
    }
}

fn latest_protocol_turn_ids_in_transaction(
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

fn latest_turn_position_for_session(
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

pub(crate) fn insert_event_bundle_in_transaction(
    transaction: &Transaction<'_>,
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<(), StorageError> {
    validate_event_bundle_coherence(event, history_item, turn_item)?;
    insert_runtime_event(transaction, event)?;
    if let Some(history_item) = history_item {
        insert_history_item(transaction, history_item)?;
    }
    if let Some(turn_item) = turn_item {
        insert_turn_item(transaction, turn_item)?;
    }
    Ok(())
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
mod tests {}
