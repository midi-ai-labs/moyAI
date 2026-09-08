use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, params};

use crate::error::StorageError;
use crate::session::{ChangeId, ChangeRepository};

#[derive(Clone)]
pub struct SqliteChangeRepository {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteChangeRepository {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    /// Only changes referenced by this settled turn's canonical FileChange items.
    /// Unreferenced tracker rows and other sessions never become remote outputs.
    pub(crate) fn terminal_changes_for_job(
        &self,
        session: crate::session::SessionId,
        turn: crate::protocol::TurnId,
    ) -> Result<Vec<crate::edit::FileChange>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let terminal: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM protocol_runtime_events WHERE session_id=?1 AND turn_id=?2 AND json_extract(msg_json,'$.kind')='turn_terminal')",
            params![session.to_string(),turn.to_string()], |row| row.get(0))?;
        if !terminal {
            return Err(StorageError::Message(
                "artifact snapshot requires a settled turn".into(),
            ));
        }
        let mut statement = connection.prepare(
            "SELECT c.id,c.tool_call_id,c.change_kind,c.path_before,c.path_after,c.before_sha256,c.after_sha256,c.created_at_ms
             FROM protocol_history_items h
             JOIN protocol_item_append_order a ON a.session_id=h.session_id AND a.source_kind='history_item' AND a.source_id=h.id
             JOIN json_each(h.payload_json,'$.change_ids') ids
             JOIN file_changes c ON c.id=ids.value AND c.tool_call_id=json_extract(h.payload_json,'$.call_id')
             WHERE h.session_id=?1 AND h.turn_id=?2 AND json_extract(h.payload_json,'$.kind')='file_change'
             ORDER BY a.append_position,CAST(ids.key AS INTEGER) LIMIT 257")?;
        let raw = statement
            .query_map(params![session.to_string(), turn.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if raw.len() > 256 {
            return Err(StorageError::Message(
                "artifact change history exceeds its bounded snapshot".into(),
            ));
        }
        raw.into_iter()
            .map(|r| {
                let invalid = || StorageError::Message("invalid canonical artifact change".into());
                Ok(crate::edit::FileChange {
                    id: r.0.parse().map_err(|_| invalid())?,
                    tool_call_id: r.1.parse().map_err(|_| invalid())?,
                    kind: serde_json::from_value(serde_json::Value::String(r.2))?,
                    path_before: r.3.map(Into::into),
                    path_after: r.4.map(Into::into),
                    before_sha256: r.5,
                    after_sha256: r.6,
                    diff_text: String::new(),
                    summary: String::new(),
                    created_at_ms: r.7,
                })
            })
            .collect()
    }
}

#[async_trait(?Send)]
impl ChangeRepository for SqliteChangeRepository {
    async fn insert_changes(
        &self,
        changes: &[crate::edit::FileChange],
    ) -> Result<Vec<ChangeId>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        for change in changes {
            transaction.execute(
                "INSERT INTO file_changes (id, tool_call_id, change_kind, path_before, path_after, before_sha256, after_sha256, diff_text, summary_text, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    change.id.to_string(),
                    change.tool_call_id.to_string(),
                    change.kind.as_str(),
                    change.path_before.as_ref().map(|value| value.as_str()),
                    change.path_after.as_ref().map(|value| value.as_str()),
                    change.before_sha256.as_deref(),
                    change.after_sha256.as_deref(),
                    change.diff_text,
                    change.summary,
                    change.created_at_ms,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(changes.iter().map(|change| change.id).collect())
    }
}
