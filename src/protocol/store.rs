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
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        insert_runtime_event(&*connection, event)?;
        Ok(())
    }

    fn append_event_bundle(
        &self,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<(), StorageError> {
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

    fn append_history_item(&self, item: &HistoryItem) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        insert_history_item(&*connection, item)?;
        Ok(())
    }

    fn append_history_turn_bundle(
        &self,
        history_item: &HistoryItem,
        turn_item: &TurnItem,
    ) -> Result<(), StorageError> {
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
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        insert_turn_item(&*connection, item)?;
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
