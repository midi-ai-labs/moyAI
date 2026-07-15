use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::error::StorageError;

const V1_INIT: &str = include_str!("../../migrations/V1__init.sql");
const V2_INDEXES: &str = include_str!("../../migrations/V2__indexes.sql");
#[cfg(test)]
const V3_TODOS: &str = include_str!("../../migrations/V3__todos.sql");
#[cfg(test)]
const V4_SESSION_STATE: &str = include_str!("../../migrations/V4__session_state.sql");
#[cfg(test)]
const V5_TODO_GRAPH: &str = include_str!("../../migrations/V5__todo_graph.sql");
const V6_PROMPT_DISPATCH: &str = include_str!("../../migrations/V6__prompt_dispatch.sql");
#[cfg(test)]
const V7_SHELL_TOOL_RENAME: &str = include_str!("../../migrations/V7__shell_tool_rename.sql");
#[cfg(test)]
const V8_SESSION_STATE_TASK_ROUTE: &str =
    include_str!("../../migrations/V8__session_state_task_route.sql");
#[cfg(test)]
const V9_SESSION_STATE_REVIEW_HANDOFF: &str =
    include_str!("../../migrations/V9__session_state_review_handoff.sql");
#[cfg(test)]
const V10_SESSION_STATE_DOCS_ROUTE_CONTRACT: &str =
    include_str!("../../migrations/V10__session_state_docs_route_contract.sql");
const V11_REQUEST_DIAGNOSTICS: &str = include_str!("../../migrations/V11__request_diagnostics.sql");
#[cfg(test)]
const V12_SESSION_STATE_CLOSEOUT_READY_RENAME: &str =
    include_str!("../../migrations/V12__session_state_closeout_ready_rename.sql");
const V13_MESSAGE_PARTS_IMAGE: &str = include_str!("../../migrations/V13__message_parts_image.sql");
const V14_HARNESS_ENGINE: &str = include_str!("../../migrations/V14__harness_engine.sql");
#[cfg(test)]
const V15_SESSION_STATE_CONTRACT_REFS: &str =
    include_str!("../../migrations/V15__session_state_contract_refs.sql");
const V16_PROTOCOL_EVENT_STORE: &str =
    include_str!("../../migrations/V16__protocol_event_store.sql");
#[cfg(test)]
const V17_SESSION_STATE_TYPED_VERIFICATION_EVIDENCE: &str =
    include_str!("../../migrations/V17__session_state_typed_verification_evidence.sql");
const V18_SESSIONS_CANCELLED_STATUS: &str =
    include_str!("../../migrations/V18__sessions_cancelled_status.sql");
#[cfg(test)]
const V19_SESSION_STATE_TOKEN_ACCOUNTING: &str =
    include_str!("../../migrations/V19__session_state_token_accounting.sql");
const V20_PROTOCOL_ITEM_APPEND_ORDER: &str =
    include_str!("../../migrations/V20__protocol_item_append_order.sql");
const V21_SESSIONS_ARCHIVE: &str = include_str!("../../migrations/V21__sessions_archive.sql");
const V22_SESSIONS_ACCESS_MODE: &str =
    include_str!("../../migrations/V22__sessions_access_mode.sql");
const V23_SESSIONS_MEMORY_MODE: &str =
    include_str!("../../migrations/V23__sessions_memory_mode.sql");
const V24_SESSIONS_MODEL_PARAMETERS: &str =
    include_str!("../../migrations/V24__sessions_model_parameters.sql");
const V25_THREAD_GOALS: &str = include_str!("../../migrations/V25__thread_goals.sql");
const V26_SESSIONS_ACTIVE_RUN_ID: &str =
    include_str!("../../migrations/V26__sessions_active_run_id.sql");
const V27_PROTOCOL_TURN_SEQUENCE_ALLOCATORS: &str =
    include_str!("../../migrations/V27__protocol_turn_sequence_allocators.sql");
const V28_SESSIONS_ACTIVE_TURN_ID: &str =
    include_str!("../../migrations/V28__sessions_active_turn_id.sql");
const V29_SESSIONS_ACTIVE_RUN_LEASE: &str =
    include_str!("../../migrations/V29__sessions_active_run_lease.sql");
const V30_SESSION_SPAWN_EDGES: &str = include_str!("../../migrations/V30__session_spawn_edges.sql");
const V31_TOOL_CALL_DECLINED_CANCELLED_STATUS: &str =
    include_str!("../../migrations/V31__tool_call_declined_cancelled_status.sql");
const V32_DROP_LEGACY_PLANNER_AUTHORITY: &str =
    include_str!("../../migrations/V32__drop_legacy_planner_authority.sql");
const V33_CANONICAL_PROTOCOL_STORAGE: &str =
    include_str!("../../migrations/V33__canonical_protocol_storage.sql");
const V34_DROP_SESSIONS_MEMORY_MODE: &str =
    include_str!("../../migrations/V34__drop_sessions_memory_mode.sql");
const V35_DROP_SESSIONS_AWAITING_USER_STATUS: &str =
    include_str!("../../migrations/V35__drop_sessions_awaiting_user_status.sql");
const V36_DROP_LEGACY_REASONING_ITEMS: &str =
    include_str!("../../migrations/V36__drop_legacy_reasoning_items.sql");
const V37_RAW_TOOL_CALL_HISTORY: &str =
    include_str!("../../migrations/V37__raw_tool_call_history.sql");
const LEGACY_PLANNER_CUTOVER_VERSION: i64 = 32;
const CANONICAL_PROTOCOL_STORAGE_VERSION: i64 = 33;
const DROP_SESSIONS_MEMORY_MODE_VERSION: i64 = 34;
const DROP_SESSIONS_AWAITING_USER_STATUS_VERSION: i64 = 35;
const DROP_LEGACY_REASONING_ITEMS_VERSION: i64 = 36;
const RAW_TOOL_CALL_HISTORY_VERSION: i64 = 37;
const SESSION_STATUS_DOMAIN: &[&str] = &["idle", "running", "completed", "cancelled", "failed"];
const RELEASED_V18_SESSION_STATUS_DOMAIN: &[&str] = &[
    "idle",
    "running",
    "completed",
    "awaiting_user",
    "cancelled",
    "failed",
];
const TOOL_CALL_STATUS_DOMAIN: &[&str] = &[
    "pending",
    "running",
    "completed",
    "declined",
    "cancelled",
    "failed",
];

pub fn run(connection: &Connection) -> Result<(), StorageError> {
    if schema_migration_applied(connection, RAW_TOOL_CALL_HISTORY_VERSION)? {
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DROP_LEGACY_REASONING_ITEMS_VERSION)? {
        run_raw_tool_call_history_migration(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DROP_SESSIONS_AWAITING_USER_STATUS_VERSION)? {
        run_drop_legacy_reasoning_items(connection)?;
        run_raw_tool_call_history_migration(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DROP_SESSIONS_MEMORY_MODE_VERSION)? {
        run_drop_sessions_awaiting_user_status(connection)?;
        run_drop_legacy_reasoning_items(connection)?;
        run_raw_tool_call_history_migration(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, CANONICAL_PROTOCOL_STORAGE_VERSION)? {
        run_drop_sessions_memory_mode(connection)?;
        run_drop_sessions_awaiting_user_status(connection)?;
        run_drop_legacy_reasoning_items(connection)?;
        run_raw_tool_call_history_migration(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    run_released_schema_through_v30(connection)?;
    if needs_tool_call_declined_cancelled_status_migration(connection)? {
        run_tool_call_declined_cancelled_status_migration(connection)?;
    }
    if !schema_migration_applied(connection, LEGACY_PLANNER_CUTOVER_VERSION)? {
        run_legacy_planner_cutover(connection)?;
    }
    run_canonical_protocol_storage_cutover(connection)?;
    run_drop_sessions_memory_mode(connection)?;
    run_drop_sessions_awaiting_user_status(connection)?;
    run_drop_legacy_reasoning_items(connection)?;
    run_raw_tool_call_history_migration(connection)?;
    validate_canonical_protocol_storage(connection)?;
    Ok(())
}

fn run_raw_tool_call_history_migration(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        canonicalize_raw_tool_call_history(connection)?;
        connection.execute_batch(V37_RAW_TOOL_CALL_HISTORY)?;
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn run_drop_legacy_reasoning_items(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(V36_DROP_LEGACY_REASONING_ITEMS)
        .map_err(StorageError::from)
}

fn run_drop_sessions_awaiting_user_status(connection: &Connection) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration(
        connection,
        V35_DROP_SESSIONS_AWAITING_USER_STATUS,
        "V35 session awaiting-user status removal migration",
    )
}

fn run_drop_sessions_memory_mode(connection: &Connection) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration(
        connection,
        V34_DROP_SESSIONS_MEMORY_MODE,
        "V34 session memory mode removal migration",
    )
}

fn run_canonical_protocol_storage_cutover(connection: &Connection) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration(
        connection,
        V33_CANONICAL_PROTOCOL_STORAGE,
        "V33 canonical protocol storage migration",
    )
}

#[cfg(test)]
fn run_through_v36(connection: &Connection) -> Result<(), StorageError> {
    run_released_schema_through_v30(connection)?;
    if needs_tool_call_declined_cancelled_status_migration(connection)? {
        run_tool_call_declined_cancelled_status_migration(connection)?;
    }
    if !schema_migration_applied(connection, LEGACY_PLANNER_CUTOVER_VERSION)? {
        run_legacy_planner_cutover(connection)?;
    }
    run_canonical_protocol_storage_cutover(connection)?;
    run_drop_sessions_memory_mode(connection)?;
    run_drop_sessions_awaiting_user_status(connection)?;
    run_drop_legacy_reasoning_items(connection)?;
    Ok(())
}

fn validate_canonical_protocol_storage(connection: &Connection) -> Result<(), StorageError> {
    if !schema_migration_applied(connection, RAW_TOOL_CALL_HISTORY_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V37 raw tool-call history marker".to_string(),
        ));
    }
    if !schema_migration_applied(connection, DROP_LEGACY_REASONING_ITEMS_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V36 legacy reasoning item removal marker".to_string(),
        ));
    }
    for retired_table in [
        "messages",
        "message_parts",
        "session_todos",
        "session_state",
    ] {
        if table_exists(connection, retired_table)? {
            return Err(StorageError::Message(format!(
                "V33 canonical protocol storage marker exists but retired table `{retired_table}` is present"
            )));
        }
    }
    if sessions_has_memory_mode(connection)? {
        return Err(StorageError::Message(
            "V34 session memory removal marker exists but sessions.memory_mode is still present"
                .to_string(),
        ));
    }
    if !table_has_exact_status_domain(connection, "sessions", SESSION_STATUS_DOMAIN)? {
        return Err(StorageError::Message(
            "V35 session status marker exists but sessions still accepts awaiting_user or has another non-current status domain"
                .to_string(),
        ));
    }
    if !tool_calls_schema_is_canonical(connection)? {
        return Err(StorageError::Message(
            "V33 canonical protocol storage marker exists but tool_calls is not canonical"
                .to_string(),
        ));
    }
    if !file_changes_schema_is_canonical(connection)? {
        return Err(StorageError::Message(
            "V33 canonical protocol storage marker exists but file_changes is not canonical"
                .to_string(),
        ));
    }
    if legacy_reasoning_projection_row_count(connection)? != 0 {
        return Err(StorageError::Message(
            "V36 legacy reasoning item removal marker exists but retired reasoning or prompt-dispatch protocol rows remain"
                .to_string(),
        ));
    }
    validate_raw_tool_call_history(connection)?;
    Ok(())
}

fn canonicalize_raw_tool_call_history(connection: &Connection) -> Result<(), StorageError> {
    if !table_exists(connection, "protocol_history_items")? {
        return Ok(());
    }

    let turns_without_response_lineage =
        legacy_tool_call_turns_without_response_lineage(connection)?;
    for (session_id, turn_id) in turns_without_response_lineage {
        delete_protocol_turn_for_raw_tool_cutover(connection, &session_id, &turn_id)?;
    }

    let mut statement = connection
        .prepare("SELECT id, payload_json FROM protocol_history_items ORDER BY id ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let stored = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (id, payload_json) in stored {
        let payload =
            serde_json::from_str::<serde_json::Value>(&payload_json).map_err(|error| {
                StorageError::Message(format!(
                    "V37 cannot inspect protocol history item {id}: invalid JSON: {error}"
                ))
            })?;
        let Some(object) = payload.as_object() else {
            continue;
        };
        if object.get("kind").and_then(serde_json::Value::as_str) != Some("tool_call") {
            continue;
        }
        if validate_raw_tool_call_object(object).is_ok() {
            let canonical_hash = sha256_text(&payload_json);
            connection.execute(
                "UPDATE protocol_history_items SET payload_sha256 = ?1 WHERE id = ?2",
                (&canonical_hash, &id),
            )?;
            continue;
        }

        let call_id = required_non_empty_json_string(object, "call_id", &id)?;
        let response_id = required_non_empty_json_string(object, "response_id", &id)?;
        let tool_name = required_json_string(object, "tool", &id)?;
        let arguments = object.get("arguments").ok_or_else(|| {
            StorageError::Message(format!(
                "V37 tool-call history item {id} has neither canonical raw arguments_json nor legacy arguments"
            ))
        })?;
        let model_call_id = object
            .get("model_call_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        let mut canonical = serde_json::Map::new();
        canonical.insert(
            "kind".to_string(),
            serde_json::Value::String("tool_call".to_string()),
        );
        canonical.insert(
            "call_id".to_string(),
            serde_json::Value::String(call_id.to_string()),
        );
        canonical.insert(
            "response_id".to_string(),
            serde_json::Value::String(response_id.to_string()),
        );
        if !model_call_id.is_empty() {
            canonical.insert(
                "model_call_id".to_string(),
                serde_json::Value::String(model_call_id.to_string()),
            );
        }
        canonical.insert(
            "tool_name".to_string(),
            serde_json::Value::String(tool_name.to_string()),
        );
        canonical.insert(
            "arguments_json".to_string(),
            serde_json::Value::String(serde_json::to_string(arguments)?),
        );
        let canonical_json = serde_json::to_string(&serde_json::Value::Object(canonical))?;
        let canonical_hash = sha256_text(&canonical_json);
        connection.execute(
            "UPDATE protocol_history_items
             SET payload_json = ?1, payload_sha256 = ?2
             WHERE id = ?3",
            (&canonical_json, &canonical_hash, &id),
        )?;
    }
    Ok(())
}

fn legacy_tool_call_turns_without_response_lineage(
    connection: &Connection,
) -> Result<BTreeSet<(String, String)>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, session_id, turn_id, payload_json
         FROM protocol_history_items ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut affected_turns = BTreeSet::new();
    for row in rows {
        let (id, session_id, turn_id, payload_json) = row?;
        let payload =
            serde_json::from_str::<serde_json::Value>(&payload_json).map_err(|error| {
                StorageError::Message(format!(
                    "V37 cannot inspect protocol history item {id}: invalid JSON: {error}"
                ))
            })?;
        let Some(object) = payload.as_object() else {
            continue;
        };
        if object.get("kind").and_then(serde_json::Value::as_str) != Some("tool_call") {
            continue;
        }
        let has_response_lineage = object
            .get("response_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|response_id| !response_id.is_empty());
        if !has_response_lineage {
            affected_turns.insert((session_id, turn_id));
        }
    }
    Ok(affected_turns)
}

fn delete_protocol_turn_for_raw_tool_cutover(
    connection: &Connection,
    session_id: &str,
    turn_id: &str,
) -> Result<(), StorageError> {
    let owner = (session_id, turn_id);
    connection.execute(
        "DELETE FROM protocol_item_append_order WHERE session_id = ?1 AND turn_id = ?2",
        owner,
    )?;
    connection.execute(
        "DELETE FROM protocol_turn_items WHERE session_id = ?1 AND turn_id = ?2",
        owner,
    )?;
    connection.execute(
        "DELETE FROM protocol_runtime_events WHERE session_id = ?1 AND turn_id = ?2",
        owner,
    )?;
    connection.execute(
        "DELETE FROM protocol_history_items WHERE session_id = ?1 AND turn_id = ?2",
        owner,
    )?;
    connection.execute(
        "DELETE FROM protocol_turn_sequence_allocators WHERE session_id = ?1 AND turn_id = ?2",
        owner,
    )?;
    Ok(())
}

fn required_json_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    item_id: &str,
) -> Result<&'a str, StorageError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            StorageError::Message(format!(
                "V37 tool-call history item {item_id} has no string `{field}`"
            ))
        })
}

fn required_non_empty_json_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    item_id: &str,
) -> Result<&'a str, StorageError> {
    required_json_string(object, field, item_id).and_then(|value| {
        if value.is_empty() {
            Err(StorageError::Message(format!(
                "V37 tool-call history item {item_id} has an empty `{field}`"
            )))
        } else {
            Ok(value)
        }
    })
}

fn validate_raw_tool_call_history(connection: &Connection) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, payload_json, payload_sha256
         FROM protocol_history_items ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (id, payload_json, payload_sha256) = row?;
        let payload = serde_json::from_str::<serde_json::Value>(&payload_json).map_err(|error| {
            StorageError::Message(format!(
                "V37 marker exists but protocol history item {id} contains invalid JSON: {error}"
            ))
        })?;
        let Some(object) = payload.as_object() else {
            continue;
        };
        if object.get("kind").and_then(serde_json::Value::as_str) != Some("tool_call") {
            continue;
        }
        validate_raw_tool_call_object(object).map_err(|reason| {
            StorageError::Message(format!(
                "V37 marker exists but tool-call history item {id} is not canonical: {reason}"
            ))
        })?;
        let expected_hash = sha256_text(&payload_json);
        if payload_sha256 != expected_hash {
            return Err(StorageError::Message(format!(
                "V37 marker exists but tool-call history item {id} has a stale payload hash"
            )));
        }
    }
    Ok(())
}

fn validate_raw_tool_call_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    const ALLOWED_FIELDS: &[&str] = &[
        "kind",
        "call_id",
        "response_id",
        "model_call_id",
        "tool_name",
        "arguments_json",
    ];
    let unexpected = object
        .keys()
        .filter(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(format!("unexpected fields: {}", unexpected.join(", ")));
    }
    for field in ["call_id", "response_id"] {
        if object
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(format!("{field} must be a non-empty string"));
        }
    }
    for field in ["tool_name", "arguments_json"] {
        if !object.get(field).is_some_and(serde_json::Value::is_string) {
            return Err(format!("{field} must be a string"));
        }
    }
    if object
        .get("model_call_id")
        .is_some_and(|value| !value.is_string())
    {
        return Err("model_call_id must be a string when present".to_string());
    }
    Ok(())
}

fn sha256_text(value: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(value.as_bytes());
    format!("{:x}", hash.finalize())
}

fn legacy_reasoning_projection_row_count(connection: &Connection) -> Result<i64, StorageError> {
    connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM protocol_runtime_events
                 WHERE CASE
                     WHEN json_valid(msg_json)
                     THEN json_extract(msg_json, '$.kind') = 'reasoning_delta'
                     ELSE 0
                 END)
              + (SELECT COUNT(*) FROM protocol_history_items
                 WHERE CASE
                     WHEN json_valid(payload_json)
                     THEN json_extract(payload_json, '$.kind') IN ('reasoning', 'prompt_dispatch')
                     ELSE 0
                 END)
              + (SELECT COUNT(*) FROM protocol_turn_items
                 WHERE CASE
                     WHEN json_valid(payload_json)
                     THEN json_extract(payload_json, '$.kind') IN ('reasoning', 'prompt_dispatch')
                     ELSE 0
                 END)",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::from)
}

fn run_legacy_planner_cutover(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        canonicalize_update_plan_storage(connection)?;
        canonicalize_compaction_mode_storage(connection)?;
        connection.execute_batch(V32_DROP_LEGACY_PLANNER_AUTHORITY)?;
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn canonicalize_update_plan_storage(connection: &Connection) -> Result<(), StorageError> {
    if table_exists(connection, "tool_calls")? {
        connection.execute(
            "UPDATE tool_calls SET tool_name = 'update_plan' WHERE tool_name IN ('todo_write', 'todowrite')",
            [],
        )?;
    }
    if table_exists(connection, "message_parts")? {
        connection.execute(
            "UPDATE message_parts
             SET payload_json = replace(replace(payload_json, '\"todo_write\"', '\"update_plan\"'), '\"todowrite\"', '\"update_plan\"')
             WHERE payload_json LIKE '%todo_write%' OR payload_json LIKE '%todowrite%'",
            [],
        )?;
    }
    for (table, column) in [
        ("protocol_runtime_events", "msg_json"),
        ("protocol_history_items", "payload_json"),
        ("protocol_turn_items", "payload_json"),
    ] {
        canonicalize_hashed_json_column(
            connection,
            table,
            column,
            &[
                ("\"todo_write\"", "\"update_plan\""),
                ("\"todowrite\"", "\"update_plan\""),
            ],
        )?;
    }
    Ok(())
}

fn canonicalize_compaction_mode_storage(connection: &Connection) -> Result<(), StorageError> {
    for (table, column) in [
        ("protocol_runtime_events", "msg_json"),
        ("protocol_history_items", "payload_json"),
        ("protocol_turn_items", "payload_json"),
    ] {
        canonicalize_hashed_json_column(
            connection,
            table,
            column,
            &[
                ("\"mode\":\"manual\"", "\"mode\":\"automatic\""),
                ("\"mode\":\"pre_turn\"", "\"mode\":\"automatic\""),
                ("\"mode\":\"mid_turn\"", "\"mode\":\"automatic\""),
            ],
        )?;
    }
    Ok(())
}

fn canonicalize_hashed_json_column(
    connection: &Connection,
    table: &str,
    column: &str,
    replacements: &[(&str, &str)],
) -> Result<(), StorageError> {
    if !table_exists(connection, table)? {
        return Ok(());
    }
    let query = format!("SELECT id, {column} FROM {table}");
    let mut statement = connection.prepare(&query)?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let values = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let update = format!("UPDATE {table} SET {column} = ?1, payload_sha256 = ?2 WHERE id = ?3");
    for (id, value) in values {
        let canonical = replacements
            .iter()
            .fold(value.clone(), |current, (from, to)| {
                current.replace(from, to)
            });
        if canonical == value {
            continue;
        }
        let mut hash = Sha256::new();
        hash.update(canonical.as_bytes());
        let digest = format!("{:x}", hash.finalize());
        connection.execute(&update, (&canonical, &digest, &id))?;
    }
    Ok(())
}

fn run_released_schema_through_v30(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch(V1_INIT)?;
    connection.execute_batch(V2_INDEXES)?;
    if needs_prompt_dispatch_migration(connection)? {
        connection.execute_batch(V6_PROMPT_DISPATCH)?;
    }
    if needs_request_diagnostics_migration(connection)? {
        connection.execute_batch(V11_REQUEST_DIAGNOSTICS)?;
    }
    if needs_message_parts_image_migration(connection)? {
        connection.execute_batch(V13_MESSAGE_PARTS_IMAGE)?;
    }
    connection.execute_batch(V14_HARNESS_ENGINE)?;
    connection.execute_batch(V16_PROTOCOL_EVENT_STORE)?;
    if needs_sessions_cancelled_status_migration(connection)? {
        connection.execute_batch(V18_SESSIONS_CANCELLED_STATUS)?;
    }
    connection.execute_batch(V20_PROTOCOL_ITEM_APPEND_ORDER)?;
    if needs_sessions_archive_migration(connection)? {
        connection.execute_batch(V21_SESSIONS_ARCHIVE)?;
    }
    if needs_sessions_access_mode_migration(connection)? {
        connection.execute_batch(V22_SESSIONS_ACCESS_MODE)?;
    }
    if needs_sessions_memory_mode_migration(connection)? {
        connection.execute_batch(V23_SESSIONS_MEMORY_MODE)?;
    }
    if needs_sessions_model_parameters_migration(connection)? {
        connection.execute_batch(V24_SESSIONS_MODEL_PARAMETERS)?;
    }
    connection.execute_batch(V25_THREAD_GOALS)?;
    if needs_sessions_active_run_id_migration(connection)? {
        connection.execute_batch(V26_SESSIONS_ACTIVE_RUN_ID)?;
    }
    connection.execute_batch(V27_PROTOCOL_TURN_SEQUENCE_ALLOCATORS)?;
    if needs_sessions_active_turn_id_migration(connection)? {
        connection.execute_batch(V28_SESSIONS_ACTIVE_TURN_ID)?;
    }
    if needs_sessions_active_run_lease_migration(connection)? {
        connection.execute_batch(V29_SESSIONS_ACTIVE_RUN_LEASE)?;
    }
    connection.execute_batch(V30_SESSION_SPAWN_EDGES)?;
    Ok(())
}

fn table_exists(connection: &Connection, table_name: &str) -> Result<bool, StorageError> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table_name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn schema_migration_applied(connection: &Connection, version: i64) -> Result<bool, StorageError> {
    if !table_exists(connection, "moyai_schema_migrations")? {
        return Ok(false);
    }
    Ok(connection
        .query_row(
            "SELECT 1 FROM moyai_schema_migrations WHERE version = ?1",
            [version],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

#[cfg(test)]
fn run_through_v30(connection: &Connection) -> Result<(), StorageError> {
    run_through_v29(connection)?;
    connection.execute_batch(V30_SESSION_SPAWN_EDGES)?;
    Ok(())
}

#[cfg(test)]
fn run_through_v29(connection: &Connection) -> Result<(), StorageError> {
    run_through_v25(connection)?;
    if needs_sessions_active_run_id_migration(connection)? {
        connection.execute_batch(V26_SESSIONS_ACTIVE_RUN_ID)?;
    }
    connection.execute_batch(V27_PROTOCOL_TURN_SEQUENCE_ALLOCATORS)?;
    if needs_sessions_active_turn_id_migration(connection)? {
        connection.execute_batch(V28_SESSIONS_ACTIVE_TURN_ID)?;
    }
    if needs_sessions_active_run_lease_migration(connection)? {
        connection.execute_batch(V29_SESSIONS_ACTIVE_RUN_LEASE)?;
    }
    Ok(())
}

#[cfg(test)]
fn run_through_v25(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch(V1_INIT)?;
    connection.execute_batch(V2_INDEXES)?;
    connection.execute_batch(V3_TODOS)?;
    connection.execute_batch(V4_SESSION_STATE)?;
    if needs_todo_graph_migration(connection)? {
        connection.execute_batch(V5_TODO_GRAPH)?;
    }
    if needs_prompt_dispatch_migration(connection)? {
        connection.execute_batch(V6_PROMPT_DISPATCH)?;
    }
    connection.execute_batch(V7_SHELL_TOOL_RENAME)?;
    if needs_task_route_migration(connection)? {
        connection.execute_batch(V8_SESSION_STATE_TASK_ROUTE)?;
    }
    if needs_review_handoff_migration(connection)? {
        connection.execute_batch(V9_SESSION_STATE_REVIEW_HANDOFF)?;
    }
    if needs_docs_route_contract_migration(connection)? {
        connection.execute_batch(V10_SESSION_STATE_DOCS_ROUTE_CONTRACT)?;
    }
    if needs_request_diagnostics_migration(connection)? {
        connection.execute_batch(V11_REQUEST_DIAGNOSTICS)?;
    }
    if needs_closeout_ready_rename_migration(connection)? {
        connection.execute_batch(V12_SESSION_STATE_CLOSEOUT_READY_RENAME)?;
    }
    if needs_message_parts_image_migration(connection)? {
        connection.execute_batch(V13_MESSAGE_PARTS_IMAGE)?;
    }
    connection.execute_batch(V14_HARNESS_ENGINE)?;
    if needs_session_state_contract_refs_migration(connection)? {
        connection.execute_batch(V15_SESSION_STATE_CONTRACT_REFS)?;
    }
    connection.execute_batch(V16_PROTOCOL_EVENT_STORE)?;
    if needs_session_state_typed_verification_evidence_migration(connection)? {
        connection.execute_batch(V17_SESSION_STATE_TYPED_VERIFICATION_EVIDENCE)?;
    }
    if needs_sessions_cancelled_status_migration(connection)? {
        connection.execute_batch(V18_SESSIONS_CANCELLED_STATUS)?;
    }
    if needs_session_state_token_accounting_migration(connection)? {
        connection.execute_batch(V19_SESSION_STATE_TOKEN_ACCOUNTING)?;
    }
    connection.execute_batch(V20_PROTOCOL_ITEM_APPEND_ORDER)?;
    if needs_sessions_archive_migration(connection)? {
        connection.execute_batch(V21_SESSIONS_ARCHIVE)?;
    }
    if needs_sessions_access_mode_migration(connection)? {
        connection.execute_batch(V22_SESSIONS_ACCESS_MODE)?;
    }
    if needs_sessions_memory_mode_migration(connection)? {
        connection.execute_batch(V23_SESSIONS_MEMORY_MODE)?;
    }
    if needs_sessions_model_parameters_migration(connection)? {
        connection.execute_batch(V24_SESSIONS_MODEL_PARAMETERS)?;
    }
    connection.execute_batch(V25_THREAD_GOALS)?;
    Ok(())
}

#[cfg(test)]
fn needs_todo_graph_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_todos)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.is_empty() && !columns.iter().any(|column| column == "todo_id"))
}

fn needs_prompt_dispatch_migration(connection: &Connection) -> Result<bool, StorageError> {
    let sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'message_parts'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(sql
        .as_deref()
        .map(|value| !value.contains("prompt_dispatch"))
        .unwrap_or(false))
}

#[cfg(test)]
fn needs_task_route_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "task_route"))
}

#[cfg(test)]
fn needs_review_handoff_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "review_scope_json")
        || !columns
            .iter()
            .any(|column| column == "implementation_handoff_json"))
}

#[cfg(test)]
fn needs_docs_route_contract_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns
        .iter()
        .any(|column| column == "completion_route_contract_pending")
        || !columns
            .iter()
            .any(|column| column == "completion_route_contract_summary")
        || !columns
            .iter()
            .any(|column| column == "docs_route_state_json"))
}

fn needs_request_diagnostics_migration(connection: &Connection) -> Result<bool, StorageError> {
    let sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'message_parts'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(sql
        .as_deref()
        .map(|value| !value.contains("request_diagnostics"))
        .unwrap_or(false))
}

#[cfg(test)]
fn needs_closeout_ready_rename_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns
        .iter()
        .any(|column| column == "completion_ready_text_only")
        && !columns
            .iter()
            .any(|column| column == "completion_closeout_ready"))
}

fn needs_message_parts_image_migration(connection: &Connection) -> Result<bool, StorageError> {
    let sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'message_parts'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(sql
        .as_deref()
        .map(|value| !value.contains("'image'"))
        .unwrap_or(false))
}

#[cfg(test)]
fn needs_session_state_contract_refs_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "contract_refs_json"))
}

#[cfg(test)]
fn needs_session_state_typed_verification_evidence_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns
        .iter()
        .any(|column| column == "verification_failure_cluster_json")
        || !columns
            .iter()
            .any(|column| column == "verification_requirement_refs_json"))
}

fn needs_sessions_cancelled_status_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    Ok(!table_has_exact_status_domain(
        connection,
        "sessions",
        RELEASED_V18_SESSION_STATUS_DOMAIN,
    )?)
}

#[cfg(test)]
fn needs_session_state_token_accounting_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns
        .iter()
        .any(|column| column == "token_accounting_json"))
}

fn needs_sessions_archive_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "archived_at_ms"))
}

fn needs_sessions_access_mode_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "access_mode"))
}

fn needs_sessions_memory_mode_migration(connection: &Connection) -> Result<bool, StorageError> {
    Ok(!sessions_has_memory_mode(connection)?)
}

fn sessions_has_memory_mode(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns.iter().any(|column| column == "memory_mode"))
}

fn needs_sessions_model_parameters_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns
        .iter()
        .any(|column| column == "model_parameters_json"))
}

fn needs_sessions_active_run_id_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "active_run_id"))
}

fn needs_sessions_active_turn_id_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "active_turn_id"))
}

fn needs_sessions_active_run_lease_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns
        .iter()
        .any(|column| column == "active_run_lease_expires_at_ms"))
}

fn needs_tool_call_declined_cancelled_status_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    Ok(!tool_calls_schema_is_v31(connection)?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TableColumnContract {
    name: String,
    declared_type: String,
    not_null: bool,
    default_sql: Option<String>,
    primary_key_position: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchemaToken {
    Word(String),
    StringLiteral(String),
    LeftParen,
    RightParen,
    Comma,
    Other,
}

fn tool_calls_schema_is_v31(connection: &Connection) -> Result<bool, StorageError> {
    if !table_has_exact_status_domain(connection, "tool_calls", TOOL_CALL_STATUS_DOMAIN)? {
        return Ok(false);
    }

    let expected_columns = [
        ("id", "TEXT", false, None, 1),
        ("session_id", "TEXT", true, None, 0),
        ("message_id", "TEXT", true, None, 0),
        ("tool_name", "TEXT", true, None, 0),
        ("status", "TEXT", true, None, 0),
        ("arguments_json", "TEXT", true, None, 0),
        ("title", "TEXT", false, None, 0),
        ("metadata_json", "TEXT", true, None, 0),
        ("output_text", "TEXT", false, None, 0),
        ("truncated_output_path", "TEXT", false, None, 0),
        ("error_text", "TEXT", false, None, 0),
        ("started_at_ms", "INTEGER", true, None, 0),
        ("finished_at_ms", "INTEGER", false, None, 0),
    ];
    let observed_columns = table_column_contracts(connection, "tool_calls")?;
    let columns_match = observed_columns.len() == expected_columns.len()
        && observed_columns
            .iter()
            .zip(expected_columns)
            .all(|(observed, expected)| {
                observed.name == expected.0
                    && observed.declared_type.eq_ignore_ascii_case(expected.1)
                    && observed.not_null == expected.2
                    && observed.default_sql.as_deref() == expected.3
                    && observed.primary_key_position == expected.4
            });
    if !columns_match {
        return Ok(false);
    }

    if !tool_calls_foreign_keys_are_current(connection)? {
        return Ok(false);
    }
    tool_calls_index_is_current(connection)
}

fn tool_calls_schema_is_canonical(connection: &Connection) -> Result<bool, StorageError> {
    if !table_has_exact_status_domain(connection, "tool_calls", TOOL_CALL_STATUS_DOMAIN)? {
        return Ok(false);
    }
    let expected_columns = [
        ("id", "TEXT", false, None, 1),
        ("history_item_id", "TEXT", true, None, 0),
        ("status", "TEXT", true, None, 0),
        ("truncated_output_path", "TEXT", false, None, 0),
        ("started_at_ms", "INTEGER", true, None, 0),
        ("finished_at_ms", "INTEGER", false, None, 0),
    ];
    if !columns_match(connection, "tool_calls", &expected_columns)? {
        return Ok(false);
    }
    let expected_foreign_keys = BTreeSet::from([(
        "protocol_history_items".to_string(),
        "history_item_id".to_string(),
        "id".to_string(),
        "NO ACTION".to_string(),
        "CASCADE".to_string(),
    )]);
    if table_foreign_keys(connection, "tool_calls")? != expected_foreign_keys {
        return Ok(false);
    }
    Ok(
        has_unique_index(connection, "tool_calls", &["history_item_id"])?
            && named_index_matches(
                connection,
                "tool_calls",
                "idx_tool_calls_started",
                false,
                &[("started_at_ms", false)],
            )?,
    )
}

fn file_changes_schema_is_canonical(connection: &Connection) -> Result<bool, StorageError> {
    let expected_columns = [
        ("id", "TEXT", false, None, 1),
        ("tool_call_id", "TEXT", true, None, 0),
        ("change_kind", "TEXT", true, None, 0),
        ("path_before", "TEXT", false, None, 0),
        ("path_after", "TEXT", false, None, 0),
        ("before_sha256", "TEXT", false, None, 0),
        ("after_sha256", "TEXT", false, None, 0),
        ("diff_text", "TEXT", true, None, 0),
        ("summary_text", "TEXT", true, None, 0),
        ("created_at_ms", "INTEGER", true, None, 0),
    ];
    if !columns_match(connection, "file_changes", &expected_columns)? {
        return Ok(false);
    }
    let expected_foreign_keys = BTreeSet::from([(
        "tool_calls".to_string(),
        "tool_call_id".to_string(),
        "id".to_string(),
        "NO ACTION".to_string(),
        "CASCADE".to_string(),
    )]);
    Ok(
        table_foreign_keys(connection, "file_changes")? == expected_foreign_keys
            && named_index_matches(
                connection,
                "file_changes",
                "idx_file_changes_tool_call_created",
                false,
                &[("tool_call_id", false), ("created_at_ms", false)],
            )?,
    )
}

fn columns_match(
    connection: &Connection,
    table_name: &str,
    expected: &[(&str, &str, bool, Option<&str>, i64)],
) -> Result<bool, StorageError> {
    let observed = table_column_contracts(connection, table_name)?;
    Ok(observed.len() == expected.len()
        && observed.iter().zip(expected).all(|(observed, expected)| {
            observed.name == expected.0
                && observed.declared_type.eq_ignore_ascii_case(expected.1)
                && observed.not_null == expected.2
                && observed.default_sql.as_deref() == expected.3
                && observed.primary_key_position == expected.4
        }))
}

fn table_foreign_keys(
    connection: &Connection,
    table_name: &str,
) -> Result<BTreeSet<(String, String, String, String, String)>, StorageError> {
    let sql = format!("PRAGMA foreign_key_list({table_name})");
    let mut statement = connection.prepare(&sql)?;
    Ok(statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<Result<BTreeSet<_>, _>>()?)
}

fn has_unique_index(
    connection: &Connection,
    table_name: &str,
    columns: &[&str],
) -> Result<bool, StorageError> {
    let expected = columns
        .iter()
        .map(|column| (*column).to_string())
        .collect::<Vec<_>>();
    let list_sql = format!("PRAGMA index_list({table_name})");
    let mut statement = connection.prepare(&list_sql)?;
    let indexes = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (index_name, unique) in indexes {
        if unique == 0 {
            continue;
        }
        let detail_sql = format!("PRAGMA index_info('{index_name}')");
        let mut detail = connection.prepare(&detail_sql)?;
        let observed = detail
            .query_map([], |row| row.get::<_, String>(2))?
            .collect::<Result<Vec<_>, _>>()?;
        if observed == expected {
            return Ok(true);
        }
    }
    Ok(false)
}

fn named_index_matches(
    connection: &Connection,
    table_name: &str,
    index_name: &str,
    expected_unique: bool,
    expected_columns: &[(&str, bool)],
) -> Result<bool, StorageError> {
    let index: Option<(i64, String, i64)> = connection
        .query_row(
            "SELECT [unique], origin, partial
             FROM pragma_index_list(?1)
             WHERE name = ?2",
            (table_name, index_name),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((unique, origin, partial)) = index else {
        return Ok(false);
    };
    if (unique != 0) != expected_unique || origin != "c" || partial != 0 {
        return Ok(false);
    }
    let mut statement = connection.prepare(
        "SELECT name, [desc]
         FROM pragma_index_xinfo(?1)
         WHERE key = 1
         ORDER BY seqno ASC",
    )?;
    let observed = statement
        .query_map([index_name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = expected_columns
        .iter()
        .map(|(name, descending)| ((*name).to_string(), *descending))
        .collect::<Vec<_>>();
    Ok(observed == expected)
}

fn table_has_exact_status_domain(
    connection: &Connection,
    table_name: &str,
    expected: &[&str],
) -> Result<bool, StorageError> {
    let sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table_name],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(false);
    };
    let domains = status_check_domains(&sql);
    if domains.len() != 1 {
        return Ok(false);
    }
    let expected = expected
        .iter()
        .map(|value| (*value).to_string())
        .collect::<BTreeSet<_>>();
    Ok(domains[0].len() == expected.len() && domains[0] == expected)
}

fn table_column_contracts(
    connection: &Connection,
    table_name: &str,
) -> Result<Vec<TableColumnContract>, StorageError> {
    let sql = format!("PRAGMA table_info({table_name})");
    let mut statement = connection.prepare(&sql)?;
    let columns = statement
        .query_map([], |row| {
            Ok(TableColumnContract {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                default_sql: row.get(4)?,
                primary_key_position: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns)
}

fn tool_calls_foreign_keys_are_current(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA foreign_key_list(tool_calls)")?;
    let keys = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<Result<BTreeSet<_>, _>>()?;
    let expected = BTreeSet::from([
        (
            "messages".to_string(),
            "message_id".to_string(),
            "id".to_string(),
            "NO ACTION".to_string(),
            "NO ACTION".to_string(),
        ),
        (
            "sessions".to_string(),
            "session_id".to_string(),
            "id".to_string(),
            "NO ACTION".to_string(),
            "NO ACTION".to_string(),
        ),
    ]);
    Ok(keys == expected)
}

fn tool_calls_index_is_current(connection: &Connection) -> Result<bool, StorageError> {
    let index: Option<(i64, String, i64)> = connection
        .query_row(
            "SELECT [unique], origin, partial
             FROM pragma_index_list('tool_calls')
             WHERE name = 'idx_tool_calls_session_started'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((unique, origin, partial)) = index else {
        return Ok(false);
    };
    if unique != 0 || origin != "c" || partial != 0 {
        return Ok(false);
    }
    let mut statement = connection.prepare(
        "SELECT name, [desc]
         FROM pragma_index_xinfo('idx_tool_calls_session_started')
         WHERE key = 1
         ORDER BY seqno ASC",
    )?;
    let columns = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns
        == vec![
            ("session_id".to_string(), 0),
            ("started_at_ms".to_string(), 0),
        ])
}

fn status_check_domains(sql: &str) -> Vec<BTreeSet<String>> {
    let tokens = tokenize_schema(sql);
    let mut domains = Vec::new();
    let mut index = 0;
    while index + 5 < tokens.len() {
        if !matches!(&tokens[index], SchemaToken::Word(word) if word == "check")
            || tokens[index + 1] != SchemaToken::LeftParen
            || !matches!(&tokens[index + 2], SchemaToken::Word(word) if word == "status")
            || !matches!(&tokens[index + 3], SchemaToken::Word(word) if word == "in")
            || tokens[index + 4] != SchemaToken::LeftParen
        {
            index += 1;
            continue;
        }
        let mut cursor = index + 5;
        let mut values = BTreeSet::new();
        let mut expect_value = true;
        let mut valid = true;
        loop {
            match tokens.get(cursor) {
                Some(SchemaToken::StringLiteral(value)) if expect_value => {
                    if !values.insert(value.clone()) {
                        valid = false;
                    }
                    expect_value = false;
                    cursor += 1;
                }
                Some(SchemaToken::Comma) if !expect_value => {
                    expect_value = true;
                    cursor += 1;
                }
                Some(SchemaToken::RightParen) if !expect_value => {
                    cursor += 1;
                    break;
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && tokens.get(cursor) == Some(&SchemaToken::RightParen) {
            domains.push(values);
        }
        index = cursor.saturating_add(1);
    }
    domains
}

fn tokenize_schema(sql: &str) -> Vec<SchemaToken> {
    let chars = sql.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }
        if character == '-' && chars.get(index + 1) == Some(&'-') {
            index += 2;
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if character == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                index += 1;
            }
            index = (index + 2).min(chars.len());
            continue;
        }
        match character {
            '(' => {
                tokens.push(SchemaToken::LeftParen);
                index += 1;
            }
            ')' => {
                tokens.push(SchemaToken::RightParen);
                index += 1;
            }
            ',' => {
                tokens.push(SchemaToken::Comma);
                index += 1;
            }
            '\'' => {
                index += 1;
                let mut value = String::new();
                while index < chars.len() {
                    if chars[index] == '\'' {
                        if chars.get(index + 1) == Some(&'\'') {
                            value.push('\'');
                            index += 2;
                            continue;
                        }
                        index += 1;
                        break;
                    }
                    value.push(chars[index]);
                    index += 1;
                }
                tokens.push(SchemaToken::StringLiteral(value));
            }
            '"' | '`' => {
                let delimiter = character;
                index += 1;
                let mut value = String::new();
                while index < chars.len() && chars[index] != delimiter {
                    value.push(chars[index]);
                    index += 1;
                }
                index = (index + 1).min(chars.len());
                tokens.push(SchemaToken::Word(value.to_ascii_lowercase()));
            }
            _ if character.is_ascii_alphanumeric() || character == '_' => {
                let start = index;
                index += 1;
                while index < chars.len()
                    && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
                {
                    index += 1;
                }
                tokens.push(SchemaToken::Word(
                    chars[start..index]
                        .iter()
                        .collect::<String>()
                        .to_ascii_lowercase(),
                ));
            }
            _ => {
                tokens.push(SchemaToken::Other);
                index += 1;
            }
        }
    }
    tokens
}

fn run_tool_call_declined_cancelled_status_migration(
    connection: &Connection,
) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration(
        connection,
        V31_TOOL_CALL_DECLINED_CANCELLED_STATUS,
        "V31 tool call status migration",
    )
}

fn run_foreign_keys_disabled_migration(
    connection: &Connection,
    migration_sql: &str,
    migration_name: &str,
) -> Result<(), StorageError> {
    if !connection.is_autocommit() {
        return Err(StorageError::Message(format!(
            "{migration_name} requires an autocommit connection"
        )));
    }
    let foreign_keys_before =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))?;
    connection.pragma_update(None, "foreign_keys", 0)?;

    let migration_result = connection.execute_batch(migration_sql);
    let rollback_error = if connection.is_autocommit() {
        None
    } else {
        connection
            .execute_batch("ROLLBACK")
            .err()
            .map(|error| error.to_string())
    };
    let restore_error = connection
        .pragma_update(None, "foreign_keys", foreign_keys_before)
        .err()
        .map(|error| error.to_string());
    let foreign_keys_after =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0));

    let mut cleanup_errors = Vec::new();
    if let Some(error) = rollback_error {
        cleanup_errors.push(format!("rollback failed: {error}"));
    }
    if !connection.is_autocommit() {
        cleanup_errors.push("migration transaction remains active".to_string());
    }
    if let Some(error) = restore_error {
        cleanup_errors.push(format!("foreign key restoration failed: {error}"));
    } else {
        match foreign_keys_after {
            Ok(value) if value == foreign_keys_before => {}
            Ok(value) => cleanup_errors.push(format!(
                "foreign key restoration did not take effect (expected {foreign_keys_before}, got {value})"
            )),
            Err(error) => cleanup_errors.push(format!(
                "foreign key restoration verification failed: {error}"
            )),
        }
    }

    match (migration_result, cleanup_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Err(error), true) => Err(StorageError::Sqlite(error)),
        (Ok(()), false) => Err(StorageError::Message(format!(
            "{migration_name} cleanup failed: {}",
            cleanup_errors.join("; ")
        ))),
        (Err(error), false) => Err(StorageError::Message(format!(
            "{migration_name} failed: {error}; cleanup failed: {}",
            cleanup_errors.join("; ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;

    fn insert_tool_call_parent_rows(connection: &Connection) {
        connection
            .execute_batch(
                "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('tool-project', 'C:/tool-workspace', 'tool workspace', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('tool-session', 'tool-project', 'tool session', 'running',
                  'C:/tool-workspace', 'model', 'http://localhost', 2, 2, NULL);
                 INSERT INTO messages
                 (id, session_id, parent_message_id, role, sequence_no, metadata_json, created_at_ms)
                 VALUES
                 ('tool-message', 'tool-session', NULL, 'assistant', 1, '{}', 3);",
            )
            .expect("tool call parent rows");
    }

    fn insert_canonical_tool_call_parent_rows(connection: &Connection) {
        connection
            .execute_batch(
                "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('tool-project', 'C:/tool-workspace', 'tool workspace', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('tool-session', 'tool-project', 'tool session', 'running',
                  'C:/tool-workspace', 'model', 'http://localhost', 2, 2, NULL);",
            )
            .expect("canonical tool call parent rows");
    }

    fn insert_tool_call(
        connection: &Connection,
        id: &str,
        status: &str,
        started_at_ms: i64,
    ) -> rusqlite::Result<usize> {
        connection.execute(
            "INSERT INTO tool_calls
             (id, session_id, message_id, tool_name, status, arguments_json, title,
              metadata_json, output_text, truncated_output_path, error_text,
              started_at_ms, finished_at_ms)
             VALUES (?1, 'tool-session', 'tool-message', 'shell', ?2, ?3, ?4,
                     ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                status,
                format!("{{\"id\":\"{id}\"}}"),
                format!("{id} title"),
                format!("{{\"status\":\"{status}\"}}"),
                format!("{id} output"),
                format!("C:/truncation/{id}.txt"),
                format!("{id} error"),
                started_at_ms,
                started_at_ms + 1,
            ],
        )
    }

    fn insert_canonical_tool_call(
        connection: &Connection,
        id: &str,
        status: &str,
        sequence_no: i64,
    ) -> rusqlite::Result<usize> {
        let history_item_id = format!("history-{id}");
        let payload = format!(
            "{{\"kind\":\"tool_call\",\"call_id\":\"{id}\",\"response_id\":\"response-{id}\",\"tool_name\":\"shell\",\"arguments_json\":\"{{}}\"}}"
        );
        let payload_hash = sha256_text(&payload);
        connection.execute(
            "INSERT INTO protocol_history_items
             (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
             VALUES (?1, 'tool-session', 'turn', ?2, ?3, ?4, ?5)",
            params![
                history_item_id,
                sequence_no,
                payload,
                payload_hash,
                sequence_no
            ],
        )?;
        connection.execute(
            "INSERT INTO tool_calls
             (id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                history_item_id,
                status,
                format!("C:/truncation/{id}.txt"),
                sequence_no,
                sequence_no + 1,
            ],
        )
    }

    fn foreign_key_violations(connection: &Connection) -> Vec<(String, i64, String, i64)> {
        let mut statement = connection
            .prepare("PRAGMA foreign_key_check")
            .expect("foreign key check");
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("foreign key rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("foreign key violations")
    }

    fn foreign_keys_setting(connection: &Connection) -> i64 {
        connection
            .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
            .expect("foreign keys setting")
    }

    fn replace_empty_tool_calls_schema(
        connection: &Connection,
        status_domain: &str,
        title_definition: &str,
        session_reference: &str,
        index_columns: Option<&str>,
    ) {
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .expect("disable foreign keys for schema fixture");
        connection
            .execute_batch(
                "DROP INDEX IF EXISTS idx_tool_calls_session_started; DROP TABLE tool_calls;",
            )
            .expect("drop tool_calls fixture");
        let index_sql = index_columns.map_or_else(String::new, |columns| {
            format!("CREATE INDEX idx_tool_calls_session_started ON tool_calls({columns});")
        });
        connection
            .execute_batch(&format!(
                "CREATE TABLE tool_calls (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL {session_reference},
                    message_id TEXT NOT NULL REFERENCES messages(id),
                    tool_name TEXT NOT NULL,
                    status TEXT NOT NULL CHECK (status IN ({status_domain})),
                    arguments_json TEXT NOT NULL,
                    {title_definition},
                    metadata_json TEXT NOT NULL,
                    output_text TEXT,
                    truncated_output_path TEXT,
                    error_text TEXT,
                    started_at_ms INTEGER NOT NULL,
                    finished_at_ms INTEGER
                );
                {index_sql}"
            ))
            .expect("replacement tool_calls fixture");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("restore foreign keys for schema fixture");
    }

    #[test]
    fn fresh_database_uses_only_canonical_protocol_storage_and_never_recreates_legacy_tables() {
        let connection = Connection::open_in_memory().expect("database");

        run(&connection).expect("fresh current schema");
        run(&connection).expect("idempotent current schema");

        for retired_table in [
            "messages",
            "message_parts",
            "session_state",
            "session_todos",
        ] {
            assert!(
                !table_exists(&connection, retired_table).expect("retired table lookup"),
                "retired table was recreated: {retired_table}"
            );
        }
        assert!(
            schema_migration_applied(&connection, LEGACY_PLANNER_CUTOVER_VERSION)
                .expect("cutover marker")
        );
        assert!(
            schema_migration_applied(&connection, CANONICAL_PROTOCOL_STORAGE_VERSION)
                .expect("canonical cutover marker")
        );
        assert!(
            schema_migration_applied(&connection, DROP_SESSIONS_MEMORY_MODE_VERSION)
                .expect("session memory removal marker")
        );
        assert!(
            schema_migration_applied(&connection, DROP_SESSIONS_AWAITING_USER_STATUS_VERSION,)
                .expect("session awaiting-user removal marker")
        );
        assert!(
            schema_migration_applied(&connection, DROP_LEGACY_REASONING_ITEMS_VERSION)
                .expect("legacy reasoning item removal marker")
        );
        assert!(
            schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("raw tool-call history marker")
        );
        assert_eq!(
            legacy_reasoning_projection_row_count(&connection)
                .expect("retired reasoning projection count"),
            0
        );
        assert!(!sessions_has_memory_mode(&connection).expect("session columns"));
        assert!(
            table_has_exact_status_domain(&connection, "sessions", SESSION_STATUS_DOMAIN)
                .expect("current session status domain")
        );
        assert!(tool_calls_schema_is_canonical(&connection).expect("tool call schema"));
        assert!(file_changes_schema_is_canonical(&connection).expect("file change schema"));
        assert!(table_exists(&connection, "protocol_history_items").expect("history table"));
        assert!(table_exists(&connection, "protocol_turn_items").expect("turn table"));
        assert!(table_exists(&connection, "protocol_runtime_events").expect("runtime table"));
    }

    #[test]
    fn v36_marker_rejects_reintroduced_legacy_reasoning_rows() {
        let connection = Connection::open_in_memory().expect("database");
        run(&connection).expect("fresh current schema");
        connection
            .execute(
                "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('retired-reasoning', 'session', 'turn', 0,
                         '{\"kind\":\"reasoning\",\"text\":\"retired\"}', 'sha', 1)",
                [],
            )
            .expect("reintroduce retired row");

        let error = validate_canonical_protocol_storage(&connection)
            .expect_err("V36 validation must reject retired protocol rows");
        assert!(
            error
                .to_string()
                .contains("retired reasoning or prompt-dispatch")
        );
    }

    #[test]
    fn v37_converts_legacy_typed_tool_call_to_raw_provider_fields() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_v36(&connection).expect("schema through V36");
        insert_canonical_tool_call_parent_rows(&connection);
        let legacy_arguments = serde_json::json!({
            "command": "echo {raw}",
            "nested": {"enabled": true}
        });
        let legacy_payload = serde_json::json!({
            "kind": "tool_call",
            "call_id": "legacy-call",
            "response_id": "legacy-response",
            "model_call_id": "provider-call",
            "tool": "unknown_provider_tool",
            "arguments": legacy_arguments,
            "model_arguments": {"retired": true},
            "effective_arguments": {"retired": true}
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('legacy-tool-history', 'tool-session', 'turn', 0, ?1, 'legacy-hash', 3)",
                [&legacy_payload],
            )
            .expect("legacy tool-call history");

        run(&connection).expect("V37 forward migration");
        run(&connection).expect("idempotent V37 migration");

        let (payload_json, payload_sha256) = connection
            .query_row(
                "SELECT payload_json, payload_sha256
                 FROM protocol_history_items WHERE id = 'legacy-tool-history'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("migrated raw tool-call history");
        let payload: serde_json::Value =
            serde_json::from_str(&payload_json).expect("canonical JSON");
        let object = payload.as_object().expect("canonical object");
        let expected_arguments_json = legacy_arguments.to_string();
        assert_eq!(
            object.get("tool_name").and_then(serde_json::Value::as_str),
            Some("unknown_provider_tool")
        );
        assert_eq!(
            object
                .get("arguments_json")
                .and_then(serde_json::Value::as_str),
            Some(expected_arguments_json.as_str())
        );
        assert_eq!(
            object
                .get("response_id")
                .and_then(serde_json::Value::as_str),
            Some("legacy-response")
        );
        assert_eq!(
            object
                .get("model_call_id")
                .and_then(serde_json::Value::as_str),
            Some("provider-call")
        );
        assert!(!object.contains_key("tool"));
        assert!(!object.contains_key("arguments"));
        assert!(!object.contains_key("model_arguments"));
        assert!(!object.contains_key("effective_arguments"));
        assert_eq!(payload_sha256, sha256_text(&payload_json));
        assert!(
            schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("V37 marker")
        );
    }

    #[test]
    fn v37_drops_every_projection_for_a_legacy_turn_without_response_lineage() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_v36(&connection).expect("schema through V36");
        insert_canonical_tool_call_parent_rows(&connection);
        connection
            .execute_batch(
                r#"INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES
                 ('legacy-tool-history', 'tool-session', 'legacy-turn', 0,
                  '{"kind":"tool_call","call_id":"legacy-call","tool":"read","arguments":{"path":"README.md"}}',
                  'legacy-tool-hash', 3),
                 ('legacy-output-history', 'tool-session', 'legacy-turn', 1,
                  '{"kind":"tool_output","call_id":"legacy-call","status":"completed","title":"read","output_text":"old","metadata":null}',
                  'legacy-output-hash', 4),
                 ('kept-history', 'tool-session', 'kept-turn', 0,
                  '{"kind":"error","message":"keep this turn"}', 'kept-history-hash', 5);
                 INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES
                 ('legacy-runtime', 'tool-session', 'legacy-turn', 0,
                  '{"kind":"assistant_message_committed","response_id":"old","text":"old"}',
                  'legacy-runtime-hash', 3),
                 ('kept-runtime', 'tool-session', 'kept-turn', 0,
                  '{"kind":"assistant_message_committed","response_id":"kept","text":"kept"}',
                  'kept-runtime-hash', 5);
                 INSERT INTO protocol_turn_items
                 (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
                 VALUES
                 ('legacy-turn-item', 'tool-session', 'legacy-turn', 'legacy-tool-history', 0,
                  '{"kind":"tool_status","call_id":"legacy-call","tool":"read","status":"completed","title":"read","summary":"old"}',
                  'legacy-turn-hash'),
                 ('kept-turn-item', 'tool-session', 'kept-turn', 'kept-history', 0,
                  '{"kind":"error","message":"keep"}', 'kept-turn-hash');
                 INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES
                 ('tool-session', 'legacy-turn', 0, 'runtime_event', 'legacy-runtime', 3),
                 ('tool-session', 'legacy-turn', 1, 'history_item', 'legacy-tool-history', 3),
                 ('tool-session', 'legacy-turn', 2, 'history_item', 'legacy-output-history', 4),
                 ('tool-session', 'legacy-turn', 3, 'turn_item', 'legacy-turn-item', 4),
                 ('tool-session', 'kept-turn', 0, 'runtime_event', 'kept-runtime', 5),
                 ('tool-session', 'kept-turn', 1, 'history_item', 'kept-history', 5),
                 ('tool-session', 'kept-turn', 2, 'turn_item', 'kept-turn-item', 5);
                 INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
                 VALUES ('tool-session', 'legacy-turn', 4), ('tool-session', 'kept-turn', 3);
                 INSERT INTO tool_calls
                 (id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms)
                 VALUES ('legacy-call', 'legacy-tool-history', 'completed', NULL, 3, 4);
                 INSERT INTO file_changes
                 (id, tool_call_id, change_kind, path_before, path_after, before_sha256,
                  after_sha256, diff_text, summary_text, created_at_ms)
                 VALUES ('legacy-change', 'legacy-call', 'update', 'README.md', 'README.md',
                         'before', 'after', 'diff', 'old change', 4);"#,
            )
            .expect("legacy and retained turn fixtures");

        run(&connection).expect("destructive V37 turn cutover");

        for table in [
            "protocol_runtime_events",
            "protocol_history_items",
            "protocol_turn_items",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ] {
            let removed = connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table} WHERE session_id = 'tool-session' AND turn_id = 'legacy-turn'"
                    ),
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or_else(|error| panic!("removed {table}: {error}"));
            let retained = connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table} WHERE session_id = 'tool-session' AND turn_id = 'kept-turn'"
                    ),
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or_else(|error| panic!("retained {table}: {error}"));
            assert_eq!(removed, 0, "affected table={table}");
            assert!(retained > 0, "unaffected table={table}");
        }
        for table in ["tool_calls", "file_changes"] {
            assert_eq!(
                connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap_or_else(|error| panic!("cascaded {table}: {error}")),
                0,
                "sidecar table={table}"
            );
        }
        assert!(
            schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("V37 marker")
        );
        assert!(foreign_key_violations(&connection).is_empty());
    }

    #[test]
    fn v37_rolls_back_when_a_response_linked_tool_call_has_no_convertible_arguments() {
        let connection = Connection::open_in_memory().expect("database");
        run_through_v36(&connection).expect("schema through V36");
        insert_canonical_tool_call_parent_rows(&connection);
        let invalid_payload = serde_json::json!({
            "kind": "tool_call",
            "call_id": "invalid-call",
            "response_id": "invalid-response",
            "tool_name": "read"
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('invalid-tool-history', 'tool-session', 'turn', 0, ?1, 'before-hash', 3)",
                [&invalid_payload],
            )
            .expect("invalid response-linked tool call");

        run(&connection).expect_err("invalid response-linked shape must roll back V37");
        assert!(
            !schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("rolled-back V37 marker")
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT payload_json FROM protocol_history_items
                     WHERE id = 'invalid-tool-history'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("retained invalid row after rollback"),
            invalid_payload
        );
    }

    #[test]
    fn v37_validation_accepts_invalid_provider_json_text_but_rejects_old_keys() {
        let connection = Connection::open_in_memory().expect("database");
        run(&connection).expect("fresh current schema");
        insert_canonical_tool_call_parent_rows(&connection);
        let raw_payload = serde_json::json!({
            "kind": "tool_call",
            "call_id": "raw-call",
            "response_id": "raw-response",
            "tool_name": "",
            "arguments_json": "{not-json}"
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('raw-tool-history', 'tool-session', 'turn', 0, ?1, ?2, 3)",
                (&raw_payload, sha256_text(&raw_payload)),
            )
            .expect("raw tool-call fixture");
        run(&connection).expect("raw invalid provider arguments remain valid history");

        let mut with_old_key: serde_json::Value =
            serde_json::from_str(&raw_payload).expect("raw payload");
        with_old_key
            .as_object_mut()
            .expect("raw object")
            .insert("tool".to_string(), serde_json::Value::String(String::new()));
        let with_old_key = with_old_key.to_string();
        connection
            .execute(
                "UPDATE protocol_history_items
                 SET payload_json = ?1, payload_sha256 = ?2
                 WHERE id = 'raw-tool-history'",
                (&with_old_key, sha256_text(&with_old_key)),
            )
            .expect("reintroduce old tool key");
        let error = run(&connection).expect_err("old tool key must fail V37 validation");
        assert!(error.to_string().contains("unexpected fields: tool"));
    }

    #[test]
    fn released_v31_tool_turn_is_dropped_whole_while_other_turns_survive_v37() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_v30(&connection).expect("released v30 schema");
        run_tool_call_declined_cancelled_status_migration(&connection)
            .expect("released v31 schema");
        insert_tool_call_parent_rows(&connection);
        insert_tool_call(&connection, "matched", "completed", 10).expect("matched tool");
        insert_tool_call(&connection, "unmatched", "failed", 11).expect("unmatched tool");
        connection
            .execute_batch(
                "UPDATE sessions
                 SET active_run_id = 'run', active_turn_id = 'legacy-turn',
                     active_run_lease_expires_at_ms = 999
                 WHERE id = 'tool-session';
                 INSERT INTO thread_goals
                 (thread_id, goal_id, objective, status, token_budget, tokens_used,
                  time_used_seconds, created_at_ms, updated_at_ms)
                 VALUES ('tool-session', 'goal', 'keep the goal', 'active', 100, 7, 3, 4, 5);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms, active_run_id,
                  active_turn_id, active_run_lease_expires_at_ms)
                 VALUES ('other-session', 'tool-project', 'other', 'awaiting_user',
                         'C:/tool-workspace', 'model', 'http://localhost', 3, 3, 44,
                         'other-run', 'kept-turn', 777);
                 INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES
                 ('history-match', 'tool-session', 'legacy-turn', 0,
                  '{\"kind\":\"tool_call\",\"call_id\":\"matched\",\"tool\":\"shell\",\"arguments\":{}}',
                  'sha-match', 6),
                 ('history-output', 'tool-session', 'legacy-turn', 1,
                  '{\"kind\":\"tool_output\",\"call_id\":\"matched\",\"status\":\"completed\",\"title\":\"shell\",\"output_text\":\"old\",\"metadata\":null}',
                  'sha-output', 7),
                 ('history-non-tool', 'tool-session', 'legacy-turn', 2,
                  '{\"kind\":\"error\",\"message\":\"same legacy turn\"}', 'sha-non-tool', 8),
                 ('history-compaction', 'other-session', 'kept-turn', 0,
                  '{\"kind\":\"compaction\",\"mode\":\"manual\",\"summary\":\"earlier work\",\"replacement_item_ids\":[]}',
                  'sha-compaction-legacy', 8),
                 ('history-kept', 'other-session', 'kept-turn', 1,
                  '{\"kind\":\"error\",\"message\":\"keep this turn\"}', 'sha-kept', 9),
                  ('legacy-reasoning', 'tool-session', 'legacy-turn', 3,
                   '{\"kind\":\"reasoning\",\"text\":\"retired raw trace\"}',
                   'sha-reasoning', 10),
                  ('legacy-prompt-dispatch', 'tool-session', 'legacy-turn', 4,
                   '{\"kind\":\"prompt_dispatch\",\"dispatch\":{\"text\":\"retired\"}}',
                   'sha-prompt-dispatch', 11);
                 INSERT INTO protocol_turn_items
                 (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
                 VALUES
                 ('affected-turn-item', 'tool-session', 'legacy-turn', 'history-match', 0,
                  '{\"kind\":\"tool_status\",\"call_id\":\"matched\",\"tool\":\"shell\",\"status\":\"completed\",\"title\":\"shell\",\"summary\":\"old\"}',
                  'sha-affected-turn'),
                 ('legacy-reasoning-turn', 'tool-session', 'legacy-turn', 'legacy-reasoning', 1,
                  '{\"kind\":\"reasoning\",\"text\":\"retired raw trace\"}', 'sha-reasoning-turn'),
                 ('legacy-prompt-turn', 'tool-session', 'legacy-turn', 'legacy-prompt-dispatch', 2,
                  '{\"kind\":\"prompt_dispatch\",\"summary\":\"retired\"}', 'sha-prompt-turn'),
                 ('kept-turn-item', 'other-session', 'kept-turn', 'history-kept', 0,
                  '{\"kind\":\"error\",\"message\":\"keep\"}', 'sha-kept-turn');
                 INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES
                 ('affected-runtime', 'tool-session', 'legacy-turn', 0,
                  '{\"kind\":\"assistant_message_committed\",\"response_id\":\"old\",\"text\":\"old\"}',
                  'sha-affected-runtime', 9),
                 ('legacy-reasoning-runtime', 'tool-session', 'legacy-turn', 1,
                  '{\"kind\":\"reasoning_delta\",\"response_id\":\"01J00000000000000000000000\",\"delta\":\"retired raw trace\"}',
                  'sha-reasoning-runtime', 12),
                 ('kept-runtime', 'other-session', 'kept-turn', 0,
                  '{\"kind\":\"assistant_message_committed\",\"response_id\":\"kept\",\"text\":\"kept\"}',
                  'sha-kept-runtime', 13);
                 INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES
                 ('tool-session', 'legacy-turn', 0, 'runtime_event', 'affected-runtime', 9),
                 ('tool-session', 'legacy-turn', 1, 'runtime_event', 'legacy-reasoning-runtime', 12),
                 ('tool-session', 'legacy-turn', 2, 'history_item', 'history-match', 6),
                 ('tool-session', 'legacy-turn', 3, 'history_item', 'history-output', 7),
                 ('tool-session', 'legacy-turn', 4, 'history_item', 'history-non-tool', 8),
                 ('tool-session', 'legacy-turn', 5, 'history_item', 'legacy-reasoning', 10),
                 ('tool-session', 'legacy-turn', 6, 'history_item', 'legacy-prompt-dispatch', 11),
                 ('tool-session', 'legacy-turn', 7, 'turn_item', 'affected-turn-item', 9),
                 ('tool-session', 'legacy-turn', 8, 'turn_item', 'legacy-reasoning-turn', 10),
                 ('tool-session', 'legacy-turn', 9, 'turn_item', 'legacy-prompt-turn', 11),
                 ('other-session', 'kept-turn', 0, 'runtime_event', 'kept-runtime', 13),
                 ('other-session', 'kept-turn', 1, 'history_item', 'history-compaction', 8),
                 ('other-session', 'kept-turn', 2, 'history_item', 'history-kept', 9),
                 ('other-session', 'kept-turn', 3, 'turn_item', 'kept-turn-item', 9);
                 INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
                 VALUES ('tool-session', 'legacy-turn', 10), ('other-session', 'kept-turn', 4);
                 INSERT INTO file_changes
                 (id, session_id, tool_call_id, change_kind, path_before, path_after,
                  before_sha256, after_sha256, diff_text, summary_text, created_at_ms)
                 VALUES
                 ('matched-change', 'tool-session', 'matched', 'update', 'before', 'after',
                  'before-sha', 'after-sha', 'diff', 'summary', 12),
                 ('unmatched-change', 'tool-session', 'unmatched', 'delete', 'old', NULL,
                  'old-sha', NULL, 'deleted', 'discard with unmatched tool', 13);
                 INSERT INTO session_state
                 (session_id, phase, active_targets_json, failure_targets_json,
                  verification_commands_json, verification_failures_json,
                  completion_closeout_ready, completion_open_work_count,
                  completion_verification_pending, updated_at_ms)
                 VALUES ('tool-session', 'editing', '[]', '[]', '[]', '[]', 0, 1, 0, 7);
                 INSERT INTO session_todos
                 (session_id, todo_id, position, content, kind, status, priority, targets_json,
                  depends_on_json, success_criteria_json, blocked_by_json)
                 VALUES ('tool-session', 'legacy-todo', 0, 'remove me', 'work', 'pending',
                         'medium', '[]', '[]', '[]', '[]');",
            )
            .expect("released v31 fixture");

        run(&connection).expect("current cutover");
        run(&connection).expect("idempotent current cutover");

        for retired_table in [
            "messages",
            "message_parts",
            "session_state",
            "session_todos",
        ] {
            assert!(!table_exists(&connection, retired_table).expect("retired table"));
        }
        let lifecycle = connection
            .query_row(
                "SELECT active_run_id, active_turn_id, active_run_lease_expires_at_ms
                 FROM sessions WHERE id = 'tool-session'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("preserved session lifecycle");
        assert_eq!(
            lifecycle,
            ("run".to_string(), "legacy-turn".to_string(), 999)
        );
        let migrated_awaiting_session = connection
            .query_row(
                "SELECT status, completed_at_ms, active_run_id, active_turn_id,
                        active_run_lease_expires_at_ms
                 FROM sessions WHERE id = 'other-session'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .expect("migrated awaiting-user session");
        assert_eq!(
            migrated_awaiting_session,
            (
                "running".to_string(),
                None,
                "other-run".to_string(),
                "kept-turn".to_string(),
                777,
            )
        );
        assert!(
            schema_migration_applied(&connection, DROP_SESSIONS_AWAITING_USER_STATUS_VERSION,)
                .expect("V35 marker")
        );
        assert!(
            schema_migration_applied(&connection, DROP_LEGACY_REASONING_ITEMS_VERSION)
                .expect("V36 marker")
        );
        assert!(
            schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("V37 marker")
        );
        assert_eq!(
            legacy_reasoning_projection_row_count(&connection)
                .expect("removed legacy reasoning projections"),
            0
        );
        assert!(
            table_has_exact_status_domain(&connection, "sessions", SESSION_STATUS_DOMAIN)
                .expect("V35 status domain")
        );
        for table in [
            "protocol_runtime_events",
            "protocol_history_items",
            "protocol_turn_items",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ] {
            assert_eq!(
                connection
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM {table} WHERE session_id = 'tool-session' AND turn_id = 'legacy-turn'"
                        ),
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or_else(|error| panic!("removed legacy {table}: {error}")),
                0,
                "affected table={table}"
            );
            assert!(
                connection
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM {table} WHERE session_id = 'other-session' AND turn_id = 'kept-turn'"
                        ),
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or_else(|error| panic!("retained other {table}: {error}"))
                    > 0,
                "unaffected table={table}"
            );
        }
        for table in ["tool_calls", "file_changes"] {
            assert_eq!(
                connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap_or_else(|error| panic!("removed {table}: {error}")),
                0,
                "sidecar table={table}"
            );
        }
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM thread_goals", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("preserved goal"),
            1
        );
        let (compaction_payload, compaction_hash) = connection
            .query_row(
                "SELECT payload_json, payload_sha256
                 FROM protocol_history_items WHERE id = 'history-compaction'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("canonical compaction item");
        assert!(compaction_payload.contains("\"mode\":\"automatic\""));
        assert!(!compaction_payload.contains("\"mode\":\"manual\""));
        let mut expected_hash = Sha256::new();
        expected_hash.update(compaction_payload.as_bytes());
        assert_eq!(compaction_hash, format!("{:x}", expected_hash.finalize()));
        assert!(foreign_key_violations(&connection).is_empty());

        run(&connection).expect("post-cutover runner must not recreate legacy schema");
        assert!(!table_exists(&connection, "messages").expect("message table"));
        assert!(!table_exists(&connection, "message_parts").expect("part table"));
        assert!(!sessions_has_memory_mode(&connection).expect("session columns"));
    }

    #[test]
    fn v34_drops_released_session_memory_column_without_changing_session_data() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_released_schema_through_v30(&connection).expect("released v30 schema");
        run_tool_call_declined_cancelled_status_migration(&connection)
            .expect("released v31 schema");
        run_legacy_planner_cutover(&connection).expect("v32 schema");
        run_canonical_protocol_storage_cutover(&connection).expect("v33 schema");
        assert!(sessions_has_memory_mode(&connection).expect("v33 memory column"));
        connection
            .execute_batch(
                r#"INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('project', 'C:/workspace', 'workspace', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                  memory_mode, model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES ('session', 'project', 'keep me', 'idle', 'C:/workspace', 'model',
                         'http://localhost', 'auto_review', 'disabled', '{"temperature":0.2}',
                         2, 3, NULL);"#,
            )
            .expect("v33 session fixture");

        run(&connection).expect("v34 through v37 upgrade");
        run(&connection).expect("idempotent current upgrade");

        assert!(!sessions_has_memory_mode(&connection).expect("current session columns"));
        assert!(
            schema_migration_applied(&connection, DROP_SESSIONS_MEMORY_MODE_VERSION)
                .expect("v34 marker")
        );
        assert!(
            schema_migration_applied(&connection, DROP_SESSIONS_AWAITING_USER_STATUS_VERSION,)
                .expect("v35 marker")
        );
        assert!(
            schema_migration_applied(&connection, DROP_LEGACY_REASONING_ITEMS_VERSION)
                .expect("v36 marker")
        );
        assert!(
            schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("v37 marker")
        );
        assert!(
            table_has_exact_status_domain(&connection, "sessions", SESSION_STATUS_DOMAIN)
                .expect("v35 status domain")
        );
        let session = connection
            .query_row(
                "SELECT title, access_mode, model_parameters_json, created_at_ms, updated_at_ms
                 FROM sessions WHERE id = 'session'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .expect("preserved session");
        assert_eq!(
            session,
            (
                "keep me".to_string(),
                "auto_review".to_string(),
                "{\"temperature\":0.2}".to_string(),
                2,
                3,
            )
        );
        assert!(foreign_key_violations(&connection).is_empty());
    }

    #[test]
    fn v33_rolls_back_destructive_changes_and_restores_foreign_keys_before_retry() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_v30(&connection).expect("v30 schema");
        run_tool_call_declined_cancelled_status_migration(&connection).expect("v31 schema");
        run_legacy_planner_cutover(&connection).expect("v32 schema");
        insert_tool_call_parent_rows(&connection);
        insert_tool_call(&connection, "matched", "completed", 10).expect("legacy tool");
        connection
            .execute_batch(
                "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('history-match', 'tool-session', 'turn', 0,
                         '{\"kind\":\"tool_call\",\"call_id\":\"matched\",\"response_id\":\"response-matched\",\"tool\":\"shell\",\"arguments\":{}}',
                         'sha', 6);
                 CREATE TABLE tool_calls_v33 (collision TEXT NOT NULL);",
            )
            .expect("failure fixture");

        run_canonical_protocol_storage_cutover(&connection)
            .expect_err("V33 collision must fail atomically");

        assert!(connection.is_autocommit());
        assert_eq!(foreign_keys_setting(&connection), 1);
        assert!(table_exists(&connection, "messages").expect("legacy messages after rollback"));
        assert!(table_exists(&connection, "message_parts").expect("legacy parts after rollback"));
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM tool_calls", [], |row| row
                    .get::<_, i64>(0))
                .expect("legacy tool count"),
            1
        );
        assert!(
            !schema_migration_applied(&connection, CANONICAL_PROTOCOL_STORAGE_VERSION)
                .expect("V33 marker after rollback")
        );
        assert!(foreign_key_violations(&connection).is_empty());

        connection
            .execute_batch("DROP TABLE tool_calls_v33;")
            .expect("remove collision");
        run_canonical_protocol_storage_cutover(&connection).expect("retry V33");
        run_drop_sessions_memory_mode(&connection).expect("apply V34 after retry");
        run_drop_sessions_awaiting_user_status(&connection).expect("apply V35 after retry");
        run_drop_legacy_reasoning_items(&connection).expect("apply V36 after retry");
        run_raw_tool_call_history_migration(&connection).expect("apply V37 after retry");
        validate_canonical_protocol_storage(&connection).expect("canonical storage after retry");
        assert_eq!(foreign_keys_setting(&connection), 1);
        assert!(foreign_key_violations(&connection).is_empty());
    }

    #[test]
    fn status_domain_detection_rejects_partial_extra_and_literal_only_schemas() {
        for (schema, expected_current) in [
            (
                "CREATE TABLE sessions (status TEXT NOT NULL CHECK (status IN ('idle', 'running', 'completed', 'awaiting_user', 'cancelled', 'failed')))",
                true,
            ),
            (
                "CREATE TABLE sessions (status TEXT NOT NULL CHECK (status IN ('idle', 'running', 'completed', 'awaiting_user', 'cancelled')))",
                false,
            ),
            (
                "CREATE TABLE sessions (status TEXT NOT NULL CHECK (status IN ('idle', 'running', 'completed', 'awaiting_user', 'cancelled', 'failed', 'rejected')))",
                false,
            ),
            (
                "CREATE TABLE sessions (status TEXT NOT NULL CHECK (status IN ('idle', 'running', 'completed', 'awaiting_user', 'failed')), note TEXT DEFAULT 'cancelled')",
                false,
            ),
        ] {
            let connection = Connection::open_in_memory().expect("database");
            connection.execute_batch(schema).expect("sessions schema");
            assert_eq!(
                !needs_sessions_cancelled_status_migration(&connection)
                    .expect("sessions migration readiness"),
                expected_current,
                "schema={schema}"
            );
        }
    }

    #[test]
    fn v31_rebuilds_each_wrong_structural_contract() {
        let canonical_domain =
            "'pending', 'running', 'completed', 'declined', 'cancelled', 'failed'";
        for (label, status_domain, title_definition, session_reference, index_columns) in [
            (
                "extra status",
                "'pending', 'running', 'completed', 'declined', 'cancelled', 'failed', 'rejected'",
                "title TEXT",
                "REFERENCES sessions(id)",
                Some("session_id, started_at_ms ASC"),
            ),
            (
                "wrong default",
                canonical_domain,
                "title TEXT DEFAULT 'unexpected'",
                "REFERENCES sessions(id)",
                Some("session_id, started_at_ms ASC"),
            ),
            (
                "missing foreign key",
                canonical_domain,
                "title TEXT",
                "",
                Some("session_id, started_at_ms ASC"),
            ),
            (
                "wrong index",
                canonical_domain,
                "title TEXT",
                "REFERENCES sessions(id)",
                Some("tool_name"),
            ),
            (
                "missing index",
                canonical_domain,
                "title TEXT",
                "REFERENCES sessions(id)",
                None,
            ),
        ] {
            let connection = Connection::open_in_memory().expect("database");
            connection
                .pragma_update(None, "foreign_keys", "ON")
                .expect("foreign keys");
            run_through_v30(&connection).expect("v30 schema");
            replace_empty_tool_calls_schema(
                &connection,
                status_domain,
                title_definition,
                session_reference,
                index_columns,
            );
            assert!(
                needs_tool_call_declined_cancelled_status_migration(&connection)
                    .expect("wrong schema readiness"),
                "variant={label}"
            );

            run_tool_call_declined_cancelled_status_migration(&connection)
                .unwrap_or_else(|error| panic!("repair {label}: {error}"));
            assert!(
                !needs_tool_call_declined_cancelled_status_migration(&connection)
                    .expect("repaired schema readiness"),
                "variant={label}"
            );
            assert_eq!(foreign_keys_setting(&connection), 1, "variant={label}");
            assert!(
                foreign_key_violations(&connection).is_empty(),
                "variant={label}"
            );
        }
    }

    #[test]
    fn fresh_database_accepts_all_tool_call_outcomes_and_is_idempotent() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");

        run(&connection).expect("fresh migration");
        run(&connection).expect("idempotent migration");
        insert_canonical_tool_call_parent_rows(&connection);

        for (index, status) in [
            "pending",
            "running",
            "completed",
            "declined",
            "cancelled",
            "failed",
        ]
        .into_iter()
        .enumerate()
        {
            insert_canonical_tool_call(&connection, status, status, index as i64 + 10)
                .unwrap_or_else(|error| panic!("insert {status}: {error}"));
        }
        run(&connection).expect("idempotent migration with all status rows");

        let statuses = connection
            .prepare("SELECT status FROM tool_calls ORDER BY started_at_ms")
            .expect("status query")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("status rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("statuses");
        assert_eq!(
            statuses,
            vec![
                "pending",
                "running",
                "completed",
                "declined",
                "cancelled",
                "failed",
            ]
        );
        assert!(insert_canonical_tool_call(&connection, "invalid", "unknown", 100).is_err());
        assert_eq!(foreign_keys_setting(&connection), 1);
        assert!(foreign_key_violations(&connection).is_empty());
    }

    #[test]
    fn v31_restores_a_disabled_foreign_key_setting_after_success() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .expect("disable foreign keys for caller fixture");
        assert_eq!(foreign_keys_setting(&connection), 0);
        run_through_v30(&connection).expect("v30 schema");
        assert_eq!(foreign_keys_setting(&connection), 0);

        run_tool_call_declined_cancelled_status_migration(&connection)
            .expect("v31 migration with foreign keys disabled");

        assert_eq!(foreign_keys_setting(&connection), 0);
        let tool_call_sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'tool_calls'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("tool call schema");
        assert!(tool_call_sql.contains("'declined'"));
        assert!(tool_call_sql.contains("'cancelled'"));
    }

    #[test]
    fn v31_rolls_back_a_failed_rebuild_and_restores_foreign_keys() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_v30(&connection).expect("v30 schema");
        insert_tool_call_parent_rows(&connection);
        insert_tool_call(&connection, "completed", "completed", 10).expect("v30 tool call");
        connection
            .execute(
                "INSERT INTO file_changes
                 (id, session_id, tool_call_id, change_kind, path_before, path_after,
                  before_sha256, after_sha256, diff_text, summary_text, created_at_ms)
                 VALUES
                 ('failure-change', 'tool-session', 'completed', 'update', 'before.txt',
                  'after.txt', 'before-sha', 'after-sha', 'diff', 'summary', 11)",
                [],
            )
            .expect("dependent file change");
        connection
            .execute_batch("CREATE TABLE tool_calls_v31 (collision TEXT NOT NULL);")
            .expect("failure fixture");
        let schema_before = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'tool_calls'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("schema before failed migration");
        let rows_before = connection
            .query_row(
                "SELECT id, status, arguments_json, metadata_json, started_at_ms, finished_at_ms
                 FROM tool_calls WHERE id = 'completed'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .expect("data before failed migration");

        run_tool_call_declined_cancelled_status_migration(&connection)
            .expect_err("collision must fail V31");

        assert!(connection.is_autocommit());
        assert_eq!(foreign_keys_setting(&connection), 1);
        let schema_after = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'tool_calls'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("schema after failed migration");
        let rows_after = connection
            .query_row(
                "SELECT id, status, arguments_json, metadata_json, started_at_ms, finished_at_ms
                 FROM tool_calls WHERE id = 'completed'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .expect("data after failed migration");
        assert_eq!(schema_after, schema_before);
        assert_eq!(rows_after, rows_before);
        assert!(schema_after.contains("'failed'"));
        assert!(!schema_after.contains("'declined'"));
        assert!(
            connection
                .execute("DELETE FROM tool_calls WHERE id = 'completed'", [])
                .is_err()
        );
        assert!(foreign_key_violations(&connection).is_empty());
        let index_count = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_tool_calls_session_started'
                   AND tbl_name = 'tool_calls'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("tool call index after failed migration");
        assert_eq!(index_count, 1);

        connection
            .execute_batch("DROP TABLE tool_calls_v31;")
            .expect("remove failure fixture");
        run_tool_call_declined_cancelled_status_migration(&connection)
            .expect("retry V31 after failure");
        assert_eq!(foreign_keys_setting(&connection), 1);
        assert!(foreign_key_violations(&connection).is_empty());
        insert_tool_call(&connection, "declined", "declined", 12).expect("declined after retry");
    }

    #[test]
    fn v31_migrates_released_v30_tool_calls_without_losing_data_or_relationships() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_v30(&connection).expect("v30 schema");
        insert_tool_call_parent_rows(&connection);

        for (index, status) in ["pending", "running", "completed", "failed"]
            .into_iter()
            .enumerate()
        {
            insert_tool_call(&connection, status, status, index as i64 + 20)
                .unwrap_or_else(|error| panic!("insert v30 {status}: {error}"));
        }
        connection
            .execute(
                "INSERT INTO file_changes
                 (id, session_id, tool_call_id, change_kind, path_before, path_after,
                  before_sha256, after_sha256, diff_text, summary_text, created_at_ms)
                 VALUES
                 ('change', 'tool-session', 'completed', 'update', 'before.txt', 'after.txt',
                  'before-sha', 'after-sha', 'diff', 'summary', 30)",
                [],
            )
            .expect("dependent file change");
        assert!(insert_tool_call(&connection, "pre-v31-declined", "declined", 30).is_err());

        run_tool_call_declined_cancelled_status_migration(&connection)
            .expect("v31 forward migration");

        let rows = connection
            .prepare(
                "SELECT id, status, arguments_json, title, metadata_json, output_text,
                        truncated_output_path, error_text, started_at_ms, finished_at_ms
                 FROM tool_calls ORDER BY started_at_ms",
            )
            .expect("migrated tool calls")
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })
            .expect("migrated tool rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("migrated tool data");
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
            vec!["pending", "running", "completed", "failed"]
        );
        assert_eq!(
            rows[2],
            (
                "completed".to_string(),
                "completed".to_string(),
                "{\"id\":\"completed\"}".to_string(),
                "completed title".to_string(),
                "{\"status\":\"completed\"}".to_string(),
                "completed output".to_string(),
                "C:/truncation/completed.txt".to_string(),
                "completed error".to_string(),
                22,
                23,
            )
        );

        let index_count = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_tool_calls_session_started'
                   AND tbl_name = 'tool_calls'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("tool call index");
        assert_eq!(index_count, 1);
        let file_change_tool_call = connection
            .query_row(
                "SELECT tool_call_id FROM file_changes WHERE id = 'change'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("dependent file change after migration");
        assert_eq!(file_change_tool_call, "completed");
        assert!(
            connection
                .execute("DELETE FROM tool_calls WHERE id = 'completed'", [])
                .is_err()
        );
        assert!(foreign_key_violations(&connection).is_empty());

        insert_tool_call(&connection, "declined", "declined", 40)
            .expect("declined after migration");
        insert_tool_call(&connection, "cancelled", "cancelled", 41)
            .expect("cancelled after migration");
        assert!(insert_tool_call(&connection, "invalid", "unknown", 42).is_err());
        assert!(
            !needs_tool_call_declined_cancelled_status_migration(&connection)
                .expect("v31 current schema")
        );
        let tool_call_count = connection
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("tool call count after idempotent migration");
        assert_eq!(tool_call_count, 6);
        assert_eq!(foreign_keys_setting(&connection), 1);
    }

    #[test]
    fn run_creates_session_spawn_edges_on_a_fresh_database_and_is_idempotent() {
        let connection = Connection::open_in_memory().expect("database");

        run(&connection).expect("fresh migration");
        run(&connection).expect("idempotent migration");

        let mut statement = connection
            .prepare("PRAGMA table_info(session_spawn_edges)")
            .expect("spawn edge columns");
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .expect("column rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("columns");
        assert_eq!(
            columns,
            vec![
                "root_session_id",
                "parent_session_id",
                "child_session_id",
                "agent_path",
                "task_name",
                "created_at_ms",
            ]
        );
    }

    #[test]
    fn session_spawn_edges_migrate_a_v29_database_without_changing_existing_sessions() {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_v29(&connection).expect("v29 schema");
        connection
            .execute_batch(
                "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('project', 'C:/workspace', 'workspace', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('root', 'project', 'root', 'idle', 'C:/workspace', 'model', 'http://localhost', 1, 1, NULL),
                 ('child', 'project', 'child', 'idle', 'C:/workspace', 'model', 'http://localhost', 2, 2, NULL);",
            )
            .expect("v29 fixture");
        let table_before = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'session_spawn_edges'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("table before");
        assert_eq!(table_before, 0);

        run(&connection).expect("forward migration");
        connection
            .execute(
                "INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id, agent_path, task_name, created_at_ms)
                 VALUES ('root', 'root', 'child', '/root/child', 'child', 3)",
                [],
            )
            .expect("spawn edge");

        let sessions = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("sessions");
        let edges = connection
            .query_row("SELECT COUNT(*) FROM session_spawn_edges", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("edges");
        assert_eq!(sessions, 2);
        assert_eq!(edges, 1);
    }

    #[test]
    fn run_identity_turn_and_lease_migrate_v25_schema_and_are_idempotent() {
        let connection = Connection::open_in_memory().expect("database");
        run_through_v25(&connection).expect("v25 schema");

        run(&connection).expect("forward migration");
        run(&connection).expect("idempotent migration");

        let mut statement = connection
            .prepare("PRAGMA table_info(sessions)")
            .expect("session columns");
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .expect("column rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("columns");
        assert_eq!(
            columns
                .iter()
                .filter(|column| column.as_str() == "active_run_id")
                .count(),
            1
        );
        assert_eq!(
            columns
                .iter()
                .filter(|column| column.as_str() == "active_run_lease_expires_at_ms")
                .count(),
            1
        );
        assert_eq!(
            columns
                .iter()
                .filter(|column| column.as_str() == "active_turn_id")
                .count(),
            1
        );
        let allocator_table_count = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM sqlite_master
                 WHERE type = 'table' AND name = 'protocol_turn_sequence_allocators'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("allocator table");
        assert_eq!(allocator_table_count, 1);
    }
}
