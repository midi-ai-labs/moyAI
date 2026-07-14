use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension};

use crate::error::StorageError;

const V1_INIT: &str = include_str!("../../migrations/V1__init.sql");
const V2_INDEXES: &str = include_str!("../../migrations/V2__indexes.sql");
const V3_TODOS: &str = include_str!("../../migrations/V3__todos.sql");
const V4_SESSION_STATE: &str = include_str!("../../migrations/V4__session_state.sql");
const V5_TODO_GRAPH: &str = include_str!("../../migrations/V5__todo_graph.sql");
const V6_PROMPT_DISPATCH: &str = include_str!("../../migrations/V6__prompt_dispatch.sql");
const V7_SHELL_TOOL_RENAME: &str = include_str!("../../migrations/V7__shell_tool_rename.sql");
const V8_SESSION_STATE_TASK_ROUTE: &str =
    include_str!("../../migrations/V8__session_state_task_route.sql");
const V9_SESSION_STATE_REVIEW_HANDOFF: &str =
    include_str!("../../migrations/V9__session_state_review_handoff.sql");
const V10_SESSION_STATE_DOCS_ROUTE_CONTRACT: &str =
    include_str!("../../migrations/V10__session_state_docs_route_contract.sql");
const V11_REQUEST_DIAGNOSTICS: &str = include_str!("../../migrations/V11__request_diagnostics.sql");
const V12_SESSION_STATE_CLOSEOUT_READY_RENAME: &str =
    include_str!("../../migrations/V12__session_state_closeout_ready_rename.sql");
const V13_MESSAGE_PARTS_IMAGE: &str = include_str!("../../migrations/V13__message_parts_image.sql");
const V14_HARNESS_ENGINE: &str = include_str!("../../migrations/V14__harness_engine.sql");
const V15_SESSION_STATE_CONTRACT_REFS: &str =
    include_str!("../../migrations/V15__session_state_contract_refs.sql");
const V16_PROTOCOL_EVENT_STORE: &str =
    include_str!("../../migrations/V16__protocol_event_store.sql");
const V17_SESSION_STATE_TYPED_VERIFICATION_EVIDENCE: &str =
    include_str!("../../migrations/V17__session_state_typed_verification_evidence.sql");
const V18_SESSIONS_CANCELLED_STATUS: &str =
    include_str!("../../migrations/V18__sessions_cancelled_status.sql");
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
const SESSION_STATUS_DOMAIN: &[&str] = &[
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
    run_through_v30(connection)?;
    if needs_tool_call_declined_cancelled_status_migration(connection)? {
        run_tool_call_declined_cancelled_status_migration(connection)?;
    }
    Ok(())
}

fn run_through_v30(connection: &Connection) -> Result<(), StorageError> {
    run_through_v29(connection)?;
    connection.execute_batch(V30_SESSION_SPAWN_EDGES)?;
    Ok(())
}

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

fn needs_task_route_migration(connection: &Connection) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "task_route"))
}

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

fn needs_session_state_contract_refs_migration(
    connection: &Connection,
) -> Result<bool, StorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(session_state)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "contract_refs_json"))
}

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
        SESSION_STATUS_DOMAIN,
    )?)
}

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
    let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(!columns.iter().any(|column| column == "memory_mode"))
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
    Ok(!tool_calls_schema_is_current(connection)?)
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

fn tool_calls_schema_is_current(connection: &Connection) -> Result<bool, StorageError> {
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
    if !connection.is_autocommit() {
        return Err(StorageError::Message(
            "V31 tool call status migration requires an autocommit connection".to_string(),
        ));
    }
    let foreign_keys_before =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))?;
    connection.pragma_update(None, "foreign_keys", 0)?;

    let migration_result = connection.execute_batch(V31_TOOL_CALL_DECLINED_CANCELLED_STATUS);
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
            "V31 tool call status migration cleanup failed: {}",
            cleanup_errors.join("; ")
        ))),
        (Err(error), false) => Err(StorageError::Message(format!(
            "V31 tool call status migration failed: {error}; cleanup failed: {}",
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

            run(&connection).unwrap_or_else(|error| panic!("repair {label}: {error}"));
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
        insert_tool_call_parent_rows(&connection);

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
            insert_tool_call(&connection, status, status, index as i64 + 10)
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
        assert!(insert_tool_call(&connection, "invalid", "unknown", 100).is_err());
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

        run(&connection).expect("v31 forward migration");
        run(&connection).expect("v31 idempotent migration");

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
        run(&connection).expect("idempotent migration with v31 status rows");
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
