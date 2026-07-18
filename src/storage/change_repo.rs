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
