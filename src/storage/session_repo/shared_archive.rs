//! Portable shared sessions contain data rows, never an executable SQLite database or local credentials.
use super::*;
use crate::agent::shared::SharedRunContext;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::io::Read;

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;
const TABLES: &[&str] = &[
    "protocol_runtime_events",
    "protocol_history_items",
    "protocol_turn_items",
    "protocol_turn_sequence_allocators",
    "protocol_item_append_order",
    "tool_calls",
    "file_changes",
    "session_admission_revisions",
    "shared_run_checkpoints",
];
const SESSION_COLUMNS: &str = "id,title,status,created_at_ms,updated_at_ms,completed_at_ms,active_run_id,active_turn_id,active_run_lease_expires_at_ms";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    version: u32,
    job_id: String,
    project_id: String,
    environment_id: String,
    schema_version: u32,
    session: Map<String, Value>,
    tables: BTreeMap<String, Vec<Map<String, Value>>>,
    transcript: Vec<Value>,
    #[serde(default)]
    sidecars: Vec<Sidecar>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sidecar {
    original_path: String,
    sha256: String,
    content_base64: String,
}
fn bad(message: &str) -> StorageError {
    StorageError::Message(message.into())
}
fn parse_id(value: &Value) -> Result<&str, StorageError> {
    value
        .as_str()
        .ok_or_else(|| bad("invalid shared archive identity"))
}

fn sidecar_entries(db: &Connection, session: &str) -> Result<Vec<(String, String)>, StorageError> {
    let mut stmt=db.prepare("SELECT t.truncated_output_path,COALESCE(f.local_path,t.truncated_output_path) FROM tool_calls t JOIN protocol_history_items h ON h.id=t.history_item_id LEFT JOIN shared_history_files f ON f.session_id=h.session_id AND f.original_path=t.truncated_output_path WHERE h.session_id=?1 AND t.truncated_output_path IS NOT NULL UNION SELECT original_path,local_path FROM shared_history_files WHERE session_id=?1")?;
    Ok(stmt
        .query_map([session], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}
pub(super) fn require_sidecar_capacity(
    db: &Connection,
    session: &str,
    paths: &crate::storage::StoragePaths,
    additional_bytes: usize,
) -> Result<(), StorageError> {
    let entries = sidecar_entries(db, session)?;
    if entries.len() >= 128 || additional_bytes > 8 * 1024 * 1024 {
        return Err(bad("shared output file limit exceeded"));
    }
    let mut total = additional_bytes as u64;
    for (_, local_path) in entries {
        let guarded = crate::workspace::PathGuard::trusted_internal_path(
            camino::Utf8Path::new(&local_path),
            &paths.truncation_dir,
        )
        .map_err(|e| bad(&e.to_string()))?;
        let file = crate::workspace::PathGuard::open_validated_read_file(&guarded)
            .map_err(|e| bad(&e.to_string()))?;
        let size = file.metadata()?.len();
        if size > 8 * 1024 * 1024 {
            return Err(bad("shared output file exceeds 8 MiB"));
        }
        total = total.saturating_add(size);
    }
    if total > 32 * 1024 * 1024 {
        return Err(bad(
            "shared output snapshots exceed the session's 32 MiB budget",
        ));
    }
    Ok(())
}
fn export_sidecars(
    db: &Connection,
    session: &str,
    paths: &crate::storage::StoragePaths,
) -> Result<Vec<Sidecar>, StorageError> {
    let entries = sidecar_entries(db, session)?;
    if entries.len() > 128 {
        return Err(bad("shared archive has more than 128 output files"));
    }
    let mut result = Vec::new();
    let mut total = 0usize;
    for (original_path, local_path) in entries {
        let guarded = crate::workspace::PathGuard::trusted_internal_path(
            camino::Utf8Path::new(&local_path),
            &paths.truncation_dir,
        )
        .map_err(|e| bad(&e.to_string()))?;
        let file = crate::workspace::PathGuard::open_validated_read_file(&guarded)
            .map_err(|e| bad(&e.to_string()))?;
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        total += bytes.len();
        if bytes.len() > 8 * 1024 * 1024 || total > 32 * 1024 * 1024 {
            return Err(bad("shared output files exceed archive bounds"));
        }
        result.push(Sidecar {
            original_path,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            content_base64: STANDARD.encode(bytes),
        });
    }
    Ok(result)
}
fn restore_sidecars(
    tx: &Transaction<'_>,
    session: SessionId,
    sidecars: &[Sidecar],
    paths: &crate::storage::StoragePaths,
) -> Result<(), StorageError> {
    if sidecars.len() > 128 {
        return Err(bad("shared archive has more than 128 output files"));
    }
    let mut total = 0usize;
    for sidecar in sidecars {
        if !camino::Utf8Path::new(&sidecar.original_path).is_absolute()
            || sidecar.original_path.len() > 32768
            || sidecar.sha256.len() != 64
        {
            return Err(bad("invalid archived output identity"));
        }
        let bytes = STANDARD
            .decode(&sidecar.content_base64)
            .map_err(|_| bad("invalid archived output bytes"))?;
        total += bytes.len();
        if bytes.len() > 8 * 1024 * 1024
            || total > 32 * 1024 * 1024
            || format!("{:x}", Sha256::digest(&bytes)) != sidecar.sha256
        {
            return Err(bad("archived output checksum or size mismatch"));
        }
        let local = paths
            .truncation_dir
            .join(format!("shared-{session}-{}.txt", sidecar.sha256));
        let guarded =
            crate::workspace::PathGuard::trusted_internal_path(&local, &paths.truncation_dir)
                .map_err(|e| bad(&e.to_string()))?;
        if local.exists() {
            let mut current = Vec::new();
            crate::workspace::PathGuard::open_validated_read_file(&guarded)
                .map_err(|e| bad(&e.to_string()))?
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut current)?;
            if current != bytes {
                return Err(bad(
                    "existing restored output differs from its immutable content",
                ));
            }
        } else {
            crate::tool::write_support::write_bytes_file_conditionally(
                &guarded,
                &bytes,
                None,
                |_| Ok(()),
            )
            .map_err(|e| bad(&e.to_string()))?;
        }
        tx.execute("INSERT INTO shared_history_files(session_id,original_path,local_path,sha256) VALUES(?1,?2,?3,?4)",params![session.to_string(),sidecar.original_path,local.as_str(),sidecar.sha256])?;
    }
    Ok(())
}

fn read_rows(
    db: &Connection,
    sql: &str,
    session: &str,
) -> Result<Vec<Map<String, Value>>, StorageError> {
    let mut statement = db.prepare(sql)?;
    let names = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut rows = statement.query([session])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        if result.len() >= MAX_ROWS {
            return Err(bad("shared archive row limit exceeded"));
        }
        let mut item = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index)? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(value) => Value::from(value),
                rusqlite::types::ValueRef::Real(value) => Value::from(value),
                rusqlite::types::ValueRef::Text(value) => Value::String(
                    std::str::from_utf8(value)
                        .map_err(|_| bad("shared archive text is invalid"))?
                        .to_owned(),
                ),
                rusqlite::types::ValueRef::Blob(_) => {
                    return Err(bad("unsupported shared archive binary database column"));
                }
            };
            item.insert(name.clone(), value);
        }
        result.push(item);
    }
    Ok(result)
}
fn selection(table: &str) -> String {
    match table {
        "tool_calls"=>"SELECT t.* FROM tool_calls t JOIN protocol_history_items h ON h.id=t.history_item_id WHERE h.session_id=?1 ORDER BY t.id".into(),
        "file_changes"=>"SELECT f.* FROM file_changes f JOIN tool_calls t ON t.id=f.tool_call_id JOIN protocol_history_items h ON h.id=t.history_item_id WHERE h.session_id=?1 ORDER BY f.id".into(),
        "protocol_item_append_order"=>format!("SELECT * FROM {table} WHERE session_id=?1 ORDER BY append_position"),
        _=>format!("SELECT * FROM {table} WHERE session_id=?1 ORDER BY rowid"),
    }
}
fn insert_rows(
    tx: &Transaction<'_>,
    table: &str,
    rows: &[Map<String, Value>],
) -> Result<(), StorageError> {
    // Table names are selected by this module, and columns must match the installed schema.
    let mut statement = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    drop(statement);
    for row in rows {
        let names = row
            .keys()
            .filter(|name| {
                !(table == "protocol_item_append_order" && name.as_str() == "append_position")
            })
            .collect::<Vec<_>>();
        if names.is_empty() || names.iter().any(|name| !columns.contains(*name)) {
            return Err(bad("unknown shared archive column"));
        }
        let values = names
            .iter()
            .map(|name| match &row[*name] {
                Value::Null => Ok(rusqlite::types::Value::Null),
                Value::String(value) => Ok(rusqlite::types::Value::Text(value.clone())),
                Value::Number(value) => value
                    .as_i64()
                    .map(rusqlite::types::Value::Integer)
                    .ok_or_else(|| bad("invalid archive integer")),
                _ => Err(bad("invalid shared archive database value")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let quoted = names
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(",");
        let placeholders = (0..names.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        tx.execute(
            &format!("INSERT INTO {table} ({quoted}) VALUES({placeholders})"),
            rusqlite::params_from_iter(values),
        )?;
    }
    Ok(())
}

impl SqliteSessionRepository {
    pub(crate) fn seed_shared_continuation(
        &self,
        context: &SharedRunContext,
        target: SessionId,
        paths: &crate::storage::StoragePaths,
    ) -> Result<(), StorageError> {
        let _producer = crate::storage::InternalFileProducerLease::acquire(paths)?;
        let continuation = context
            .continuation
            .as_ref()
            .ok_or_else(|| bad("shared continuation is missing"))?;
        if serde_json::to_vec(&continuation.archive)?.len() > MAX_BYTES {
            return Err(bad("shared continuation exceeds 64 MiB"));
        }
        let archive: Archive = serde_json::from_value(continuation.archive.clone())?;
        if archive.version != 1
            || archive.job_id != continuation.previous_job_id
            || archive.project_id != context.project_id
            || archive.job_id == context.job_id
        {
            return Err(bad(
                "shared continuation belongs to another business history",
            ));
        }
        let source: SessionId = parse_id(&archive.session["id"])?
            .parse()
            .map_err(|_| bad("invalid shared source session"))?;
        let histories = archive
            .tables
            .get("protocol_history_items")
            .ok_or_else(|| bad("shared source history missing"))?;
        let turns = archive
            .tables
            .get("protocol_turn_items")
            .ok_or_else(|| bad("shared source turn items missing"))?;
        let order = archive
            .tables
            .get("protocol_item_append_order")
            .ok_or_else(|| bad("shared source history order missing"))?;
        let by_id = histories
            .iter()
            .map(|row| (row["id"].as_str().unwrap_or(""), row))
            .collect::<HashMap<_, _>>();
        let mut history = Vec::new();
        for position in order {
            if position["source_kind"] != "history_item" {
                continue;
            }
            let row = by_id
                .get(position["source_id"].as_str().unwrap_or(""))
                .ok_or_else(|| bad("shared continuation history order is incomplete"))?;
            let item = HistoryItem {
                id: parse_id(&row["id"])?
                    .parse()
                    .map_err(|_| bad("invalid history identity"))?,
                session_id: parse_id(&row["session_id"])?
                    .parse()
                    .map_err(|_| bad("invalid history session"))?,
                scope: match row["scope_kind"].as_str() {
                    Some("session") => HistoryScope::Session,
                    Some("turn") => HistoryScope::Turn {
                        turn_id: parse_id(&row["turn_id"])?
                            .parse()
                            .map_err(|_| bad("invalid history turn"))?,
                    },
                    _ => return Err(bad("invalid shared history scope")),
                },
                sequence_no: row["sequence_no"]
                    .as_i64()
                    .ok_or_else(|| bad("invalid history sequence"))?,
                created_at_ms: row["created_at_ms"]
                    .as_i64()
                    .ok_or_else(|| bad("invalid history time"))?,
                payload: serde_json::from_str(parse_id(&row["payload_json"])?)?,
            };
            history.push(item);
        }
        if history.len() != histories.len() {
            return Err(bad("shared continuation has omitted or duplicated history"));
        }
        let mut turn_items = Vec::new();
        for row in turns {
            turn_items.push(TurnItem {
                id: parse_id(&row["id"])?
                    .parse()
                    .map_err(|_| bad("invalid turn item identity"))?,
                session_id: parse_id(&row["session_id"])?
                    .parse()
                    .map_err(|_| bad("invalid turn item session"))?,
                turn_id: parse_id(&row["turn_id"])?
                    .parse()
                    .map_err(|_| bad("invalid turn identity"))?,
                source_item_id: row["source_item_id"]
                    .as_str()
                    .map(str::parse)
                    .transpose()
                    .map_err(|_| bad("invalid turn source"))?,
                sequence_no: row["sequence_no"]
                    .as_i64()
                    .ok_or_else(|| bad("invalid turn sequence"))?,
                payload: serde_json::from_str(parse_id(&row["payload_json"])?)?,
            });
        }
        let mut db = self.connection.lock().expect("sqlite mutex poisoned");
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::protocol::fork_shared_history_in_transaction(
            &tx, source, target, history, turn_items,
        )?;
        restore_sidecars(&tx, target, &archive.sidecars, paths)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn export_shared_archive(
        &self,
        job: &str,
        paths: &crate::storage::StoragePaths,
    ) -> Result<Option<Value>, StorageError> {
        let mut db = self.connection.lock().expect("sqlite mutex poisoned");
        let tx = db.transaction()?;
        let binding: Option<(String,String,String)>=tx.query_row(
            "SELECT session_id,project_id,environment_id FROM shared_run_checkpoints WHERE job_id=?1",[job],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((session, project, environment)) = binding else {
            return Ok(None);
        };
        let session_row = read_rows(
            &tx,
            &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id=?1"),
            &session,
        )?
        .pop()
        .ok_or_else(|| bad("shared archive session missing"))?;
        // Shared execution currently uses one local agent. Do not silently omit a retained child tree.
        if tx.query_row("SELECT EXISTS(SELECT 1 FROM session_spawn_edges WHERE root_session_id=?1 OR child_session_id=?1)",[&session],|r|r.get::<_,bool>(0))? {
            return Err(bad("shared archive cannot omit a local child tree"));
        }
        let mut tables = BTreeMap::new();
        let mut count = 0usize;
        for &table in TABLES {
            let rows = read_rows(&tx, &selection(table), &session)?;
            count += rows.len();
            if count > MAX_ROWS {
                return Err(bad("shared archive total row limit exceeded"));
            }
            tables.insert(table.to_owned(), rows);
        }
        let mut transcript = Vec::new();
        let history = tables["protocol_history_items"]
            .iter()
            .map(|row| (row["id"].as_str().unwrap_or(""), row))
            .collect::<HashMap<_, _>>();
        for order in &tables["protocol_item_append_order"] {
            if order["source_kind"] != "history_item" {
                continue;
            }
            let row = history
                .get(order["source_id"].as_str().unwrap_or(""))
                .ok_or_else(|| bad("shared archive history order is incomplete"))?;
            let payload: Value = serde_json::from_str(
                row["payload_json"]
                    .as_str()
                    .ok_or_else(|| bad("shared archive history payload missing"))?,
            )?;
            transcript.push(json!({"kind":payload["kind"],"payload":payload}));
        }
        let schema_version = tx.query_row(
            "SELECT MAX(version) FROM moyai_schema_migrations",
            [],
            |r| r.get(0),
        )?;
        let sidecars = export_sidecars(&tx, &session, paths)?;
        let archive = Archive {
            version: 1,
            job_id: job.into(),
            project_id: project,
            environment_id: environment,
            schema_version,
            session: session_row,
            tables,
            transcript,
            sidecars,
        };
        let bytes = serde_json::to_vec(&archive)?;
        if bytes.len() > MAX_BYTES {
            return Err(bad("shared archive exceeds 64 MiB"));
        }
        tx.commit()?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// A fresh Hub attempt may restore only its missing exact paused session. Existing local
    /// history is never replaced, and a v1 digest cannot be silently changed to v2.
    pub(crate) fn restore_shared_archive(
        &self,
        context: &SharedRunContext,
        value: &Value,
        workspace: &crate::workspace::Workspace,
        config: &crate::config::ResolvedConfig,
        paths: &crate::storage::StoragePaths,
    ) -> Result<bool, StorageError> {
        let _producer = crate::storage::InternalFileProducerLease::acquire(paths)?;
        if serde_json::to_vec(value)?.len() > MAX_BYTES {
            return Err(bad("shared archive exceeds 64 MiB"));
        }
        let archive: Archive = serde_json::from_value(value.clone())?;
        let checkpoint = context
            .checkpoint()
            .map_err(StorageError::Message)?
            .ok_or_else(|| bad("shared archive restore needs a Hub checkpoint"))?;
        if archive.version != 1
            || archive.job_id != context.job_id
            || archive.project_id != context.project_id
            || archive.environment_id != context.environment_id
            || archive.session.get("id").and_then(Value::as_str)
                != Some(checkpoint.session_id.to_string().as_str())
        {
            return Err(bad("shared archive belongs to another execution"));
        }
        let mut db = self.connection.lock().expect("sqlite mutex poisoned");
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [checkpoint.session_id.to_string()],
            |r| r.get(0),
        )?;
        if exists {
            shared_runs::validate_paused(&tx, context, &checkpoint)?;
            return Ok(false);
        }
        if checkpoint.version != 2 {
            return Err(bad(
                "legacy shared checkpoint requires its original canonical store",
            ));
        }
        let schema: u32 = tx.query_row(
            "SELECT MAX(version) FROM moyai_schema_migrations",
            [],
            |r| r.get(0),
        )?;
        if archive.schema_version > schema
            || archive.tables.len() != TABLES.len()
            || TABLES
                .iter()
                .any(|table| !archive.tables.contains_key(*table))
        {
            return Err(bad("unsupported shared archive schema"));
        }
        let session = checkpoint.session_id.to_string();
        let mut count = 0usize;
        for (table, rows) in &archive.tables {
            count += rows.len();
            if count > MAX_ROWS {
                return Err(bad("shared archive row limit exceeded"));
            }
            for row in rows {
                if !matches!(table.as_str(), "tool_calls" | "file_changes")
                    && row.get("session_id").and_then(Value::as_str) != Some(&session)
                {
                    return Err(bad("shared archive contains a foreign session"));
                }
                if let Some(payload) = row.get("payload_json").or_else(|| row.get("msg_json")) {
                    if let Some(hash) = row.get("payload_sha256") {
                        let payload = payload
                            .as_str()
                            .ok_or_else(|| bad("invalid archive payload"))?;
                        if hash.as_str()
                            != Some(format!("{:x}", Sha256::digest(payload.as_bytes())).as_str())
                        {
                            return Err(bad("shared archive payload checksum mismatch"));
                        }
                    }
                }
            }
        }
        let histories = archive.tables["protocol_history_items"]
            .iter()
            .filter_map(|row| row["id"].as_str())
            .collect::<HashSet<_>>();
        let tools = archive.tables["tool_calls"]
            .iter()
            .filter_map(|row| row["id"].as_str())
            .collect::<HashSet<_>>();
        if archive.tables["tool_calls"]
            .iter()
            .any(|row| !histories.contains(row["history_item_id"].as_str().unwrap_or("")))
            || archive.tables["file_changes"]
                .iter()
                .any(|row| !tools.contains(row["tool_call_id"].as_str().unwrap_or("")))
        {
            return Err(bad("shared archive contains foreign tool ownership"));
        }
        let binding = &archive.tables["shared_run_checkpoints"];
        if binding.len() != 1
            || binding[0]["job_id"] != context.job_id
            || binding[0]["project_id"] != context.project_id
            || binding[0]["environment_id"] != context.environment_id
            || binding[0]["checkpoint_json"]
                .as_str()
                .map(serde_json::from_str::<Value>)
                .transpose()?
                != context.resume.as_ref().map(|r| r.checkpoint.clone())
        {
            return Err(bad("shared archive checkpoint binding differs from Hub"));
        }
        let mut session_row = archive.session;
        let allowed = SESSION_COLUMNS.split(',').collect::<HashSet<_>>();
        if session_row
            .keys()
            .any(|name| !allowed.contains(name.as_str()))
        {
            return Err(bad("shared archive contains local session authority"));
        }
        session_row.insert(
            "project_id".into(),
            Value::from(workspace.project_id.to_string()),
        );
        session_row.insert("cwd_path".into(), Value::from(workspace.cwd.as_str()));
        session_row.insert("model_name".into(), Value::from(config.model.model.clone()));
        session_row.insert(
            "base_url".into(),
            Value::from(config.model.base_url.clone()),
        );
        session_row.insert(
            "access_mode".into(),
            Value::from(config.permissions.access_mode.as_str()),
        );
        insert_rows(&tx, "sessions", &[session_row])?;
        // Session creation seeds revision 0. Restore the exact archived revision only after
        // history insertion has finished firing the ordinary admission revision triggers.
        for &table in TABLES {
            if table == "session_admission_revisions" {
                tx.execute(
                    "DELETE FROM session_admission_revisions WHERE session_id=?1",
                    [&session],
                )?;
            }
            insert_rows(&tx, table, &archive.tables[table])?;
        }
        restore_sidecars(&tx, checkpoint.session_id, &archive.sidecars, paths)?;
        shared_runs::validate_paused(&tx, context, &checkpoint)?;
        tx.commit()?;
        Ok(true)
    }

    pub(crate) fn shared_history_file(
        &self,
        session: SessionId,
        original: &camino::Utf8Path,
    ) -> Result<Option<(camino::Utf8PathBuf, String)>, StorageError> {
        let db = self.connection.lock().expect("sqlite mutex poisoned");
        Ok(db.query_row("SELECT local_path,sha256 FROM shared_history_files WHERE session_id=?1 AND original_path=?2",params![session.to_string(),original.as_str()],|r|Ok((camino::Utf8PathBuf::from(r.get::<_,String>(0)?),r.get(1)?))).optional()?)
    }
}
