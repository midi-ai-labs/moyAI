use rusqlite::params;

use super::*;

fn text_snapshot(connection: &Connection, sql: &str) -> String {
    connection
        .query_row(sql, [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| panic!("snapshot query failed: {error}; sql={sql}"))
}

fn v37_byte_order_snapshot(connection: &Connection) -> Vec<String> {
    [
            "SELECT json_group_array(json_array(id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)) FROM (SELECT * FROM protocol_runtime_events ORDER BY session_id, turn_id, sequence_no, id)",
            "SELECT json_group_array(json_array(id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)) FROM (SELECT * FROM protocol_history_items ORDER BY session_id, turn_id, sequence_no, id)",
            "SELECT json_group_array(json_array(id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)) FROM (SELECT * FROM protocol_turn_items ORDER BY session_id, turn_id, sequence_no, id)",
            "SELECT json_group_array(json_array(append_position, session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)) FROM (SELECT * FROM protocol_item_append_order ORDER BY append_position)",
            "SELECT json_group_array(json_array(session_id, turn_id, next_sequence_no)) FROM (SELECT * FROM protocol_turn_sequence_allocators ORDER BY session_id, turn_id)",
            "SELECT json_group_array(json_array(id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms)) FROM (SELECT * FROM tool_calls ORDER BY id)",
            "SELECT json_group_array(json_array(id, tool_call_id, change_kind, path_before, path_after, before_sha256, after_sha256, diff_text, summary_text, created_at_ms)) FROM (SELECT * FROM file_changes ORDER BY id)",
            "SELECT json_group_array(json_array(id, active_run_id, active_turn_id, active_run_lease_expires_at_ms, status)) FROM (SELECT * FROM sessions ORDER BY id)",
        ]
        .into_iter()
        .map(|sql| text_snapshot(connection, sql))
        .collect()
}

fn run_through_v38(connection: &Connection) {
    run_through_v36(connection).expect("schema through V36");
    run_raw_tool_call_history_migration(connection).expect("V37 schema");
    run_remove_auto_review_access_mode(connection).expect("V38 schema");
}

fn run_through_v39(connection: &Connection) {
    run_through_v38(connection);
    run_terminal_outcome_cutover(connection).expect("V39 schema");
}

fn run_through_v40(connection: &Connection) {
    run_through_v39(connection);
    run_flatten_session_spawn_edges(connection).expect("V40 schema");
}

fn run_through_v42(connection: &Connection) {
    run_through_v40(connection);
    run_indexed_collaboration_mode_lookup(connection).expect("V41 schema");
    run_typed_history_scope(connection).expect("V42 schema");
}

fn run_through_v43(connection: &Connection) {
    run_through_v42(connection);
    run_indexed_internal_file_ownership(connection).expect("V43 schema");
}

fn run_through_v44(connection: &Connection) {
    run_through_v43(connection);
    run_unique_turn_terminal(connection).expect("V44 schema");
}

fn run_through_v45(connection: &Connection) {
    run_through_v44(connection);
    run_restore_auto_review_access_mode(connection).expect("V45 schema");
}

fn run_through_v46(connection: &Connection) {
    run_through_v45(connection);
    run_codex_compaction_checkpoint(connection).expect("V46 schema");
}

fn insert_v46_compaction_parent(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('v46-project', 'C:/workspace', 'workspace', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES ('v46-session', 'v46-project', 'session', 'completed',
                         'C:/workspace', 'model', 'http://localhost', 2, 2, 2);",
        )
        .expect("V46 compaction parent rows");
}

fn insert_v46_compaction(
    connection: &Connection,
    id: &str,
    sequence_no: i64,
    payload_json: &str,
    payload_sha256: &str,
) {
    seed_fixture_turn_allocator(
        connection,
        "v46-session",
        "01J00000000000000000000000",
        sequence_no.saturating_add(1),
    );
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no,
                  payload_json, payload_sha256, created_at_ms)
                 VALUES (?1, 'v46-session', 'turn', '01J00000000000000000000000',
                         ?2, ?3, ?4, ?2)",
            params![id, sequence_no, payload_json, payload_sha256],
        )
        .expect("V46 compaction fixture");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES ('v46-session', 'turn', '01J00000000000000000000000', ?1,
                         'history_item', ?2, ?1)",
            params![sequence_no, id],
        )
        .expect("V46 compaction append-order fixture");
}

fn insert_v46_replacement(connection: &Connection, id: &str, sequence_no: i64) {
    let payload_json = serde_json::json!({
        "kind": "error",
        "message": "replacement evidence",
    })
    .to_string();
    insert_v46_compaction(
        connection,
        id,
        sequence_no,
        &payload_json,
        &sha256_text(&payload_json),
    );
}

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
    seed_fixture_turn_allocator(
        connection,
        "tool-session",
        "turn",
        sequence_no.saturating_add(1),
    );
    let history_item_id = format!("history-{id}");
    let payload = format!(
        "{{\"kind\":\"tool_call\",\"call_id\":\"{id}\",\"response_id\":\"response-{id}\",\"tool_name\":\"shell\",\"arguments_json\":\"{{}}\"}}"
    );
    let payload_hash = sha256_text(&payload);
    connection.execute(
            "INSERT INTO protocol_history_items
             (id, session_id, scope_kind, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
             VALUES (?1, 'tool-session', 'turn', 'turn', ?2, ?3, ?4, ?5)",
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

fn seed_fixture_turn_allocator(
    connection: &Connection,
    session_id: &str,
    turn_id: &str,
    next_sequence_no: i64,
) {
    connection
        .execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_id, turn_id) DO UPDATE SET
                     next_sequence_no = MAX(
                         protocol_turn_sequence_allocators.next_sequence_no,
                         excluded.next_sequence_no
                     )",
            params![session_id, turn_id, next_sequence_no],
        )
        .expect("fixture turn allocator");
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
    assert!(
        schema_migration_applied(&connection, TERMINAL_OUTCOME_CUTOVER_VERSION)
            .expect("terminal outcome marker")
    );
    assert!(
        schema_migration_applied(&connection, FLATTEN_SESSION_SPAWN_EDGES_VERSION)
            .expect("flat spawn-edge marker")
    );
    assert!(
        schema_migration_applied(&connection, INDEXED_COLLABORATION_MODE_LOOKUP_VERSION)
            .expect("indexed collaboration-mode marker")
    );
    assert!(
        schema_migration_applied(&connection, INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION)
            .expect("indexed internal-file ownership marker")
    );
    assert!(
        schema_migration_has_exact_name(
            &connection,
            AGENT_OWNER_RESUME_REQUESTS_VERSION,
            AGENT_OWNER_RESUME_REQUESTS_NAME,
        )
        .expect("V48 marker")
    );
    assert!(
        schema_migration_has_exact_name(
            &connection,
            AGENT_TREE_STOP_FENCES_VERSION,
            AGENT_TREE_STOP_FENCES_NAME,
        )
        .expect("V49 marker")
    );
    assert!(
        schema_migration_has_exact_name(
            &connection,
            DURABLE_AGENT_MAILBOX_VERSION,
            DURABLE_AGENT_MAILBOX_NAME,
        )
        .expect("V50 marker")
    );
    validate_durable_agent_mailbox_schema(&connection).expect("current durable mailbox schema");
    validate_agent_owner_resume_request_schema(&connection).expect("current owner-resume schema");
    validate_indexed_collaboration_mode_lookup(&connection)
        .expect("current collaboration-mode partial index");
    validate_indexed_internal_file_ownership(&connection)
        .expect("current internal-file ownership partial index");
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
    assert!(table_exists(&connection, "agent_owner_resume_requests").expect("owner-resume table"));
    assert!(table_exists(&connection, "agent_tree_stop_fences").expect("tree-stop fence table"));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM sqlite_master
                     WHERE type = 'view'
                       AND name = 'effective_agent_deferred_completions'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("effective deferred-completion view"),
        1
    );
}

#[test]
fn released_v40_database_reaches_current_storage_atomically() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v40(&connection);
    assert!(
        schema_migration_applied(&connection, FLATTEN_SESSION_SPAWN_EDGES_VERSION)
            .expect("V40 marker")
    );
    assert!(
        !schema_migration_applied(&connection, INDEXED_COLLABORATION_MODE_LOOKUP_VERSION)
            .expect("no V41 marker")
    );
    assert!(
        !schema_migration_applied(&connection, TYPED_HISTORY_SCOPE_VERSION).expect("no V42 marker")
    );
    assert!(
        !schema_migration_applied(&connection, INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION)
            .expect("no V43 marker")
    );

    run(&connection).expect("upgrade V40 through current storage");

    assert!(
        schema_migration_applied(&connection, INDEXED_COLLABORATION_MODE_LOOKUP_VERSION)
            .expect("V41 marker")
    );
    assert!(
        schema_migration_applied(&connection, TYPED_HISTORY_SCOPE_VERSION).expect("V42 marker")
    );
    assert!(
        schema_migration_applied(&connection, INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION)
            .expect("V43 marker")
    );
    assert!(
        schema_migration_applied(&connection, RECURSIVE_SESSION_SPAWN_EDGES_VERSION)
            .expect("V47 marker")
    );
    validate_indexed_collaboration_mode_lookup(&connection).expect("V41 index contract");
    validate_indexed_internal_file_ownership(&connection).expect("V43 index contract");
    validate_recursive_session_spawn_edge_schema(&connection)
        .expect("V47 recursive spawn-edge contract");
}

#[test]
fn released_v42_database_reaches_current_storage() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v42(&connection);
    assert!(
        schema_migration_applied(&connection, TYPED_HISTORY_SCOPE_VERSION).expect("V42 marker")
    );
    assert!(
        !schema_migration_applied(&connection, INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION)
            .expect("no V43 marker")
    );
    assert!(
        !schema_migration_applied(&connection, UNIQUE_TURN_TERMINAL_VERSION)
            .expect("no V44 marker")
    );

    run(&connection).expect("upgrade V42 through current storage");

    assert!(
        schema_migration_applied(&connection, INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION)
            .expect("V43 marker")
    );
    assert!(
        schema_migration_applied(&connection, UNIQUE_TURN_TERMINAL_VERSION).expect("V44 marker")
    );
    validate_indexed_internal_file_ownership(&connection).expect("V43 index contract");
    validate_unique_turn_terminal_index(&connection).expect("V44 index contract");
}

#[test]
fn v43_marker_rejects_a_stale_internal_file_ownership_index() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    connection
        .execute_batch(
            "DROP INDEX idx_tool_calls_truncated_output_path;
                 CREATE INDEX idx_tool_calls_truncated_output_path
                 ON tool_calls(truncated_output_path);",
        )
        .expect("replace current index with non-partial index");

    let error = run(&connection).expect_err("V43 marker must validate the index contract");
    assert!(error.to_string().contains("missing or stale"));
}

#[test]
fn v42_marker_rejects_a_stale_partial_index_contract() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    connection
        .execute_batch(
            "DROP INDEX idx_protocol_history_collaboration_mode_session;
                 CREATE INDEX idx_protocol_history_collaboration_mode_session
                 ON protocol_history_items(session_id, id)
                 WHERE json_valid(payload_json);",
        )
        .expect("replace current index with stale predicate");

    let error = run(&connection).expect_err("V42 marker must validate the index contract");
    assert!(error.to_string().contains("stale predicate"));
}

#[test]
fn v42_converts_mode_and_idle_mail_pseudo_turns_to_session_scope() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v40(&connection);
    run_indexed_collaboration_mode_lookup(&connection).expect("V41 schema");
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('scope-project', 'C:/scope', 'scope', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES ('scope-session', 'scope-project', 'scope', 'completed', 'C:/scope',
                         'model', 'http://localhost', 1, 1, 1);",
        )
        .expect("scope parents");

    let user_payload = serde_json::to_string(&crate::protocol::HistoryItemPayload::UserTurn {
        content: vec![crate::protocol::ContentPart::Text {
            text: "real request".to_string(),
        }],
        prompt_dispatch: None,
        editor_context: None,
    })
    .expect("user payload");
    let terminal_payload = serde_json::to_string(&crate::protocol::TurnItemPayload::Terminal {
        outcome: crate::protocol::TurnTerminalOutcome::Completed,
    })
    .expect("terminal payload");
    let mode_payload = serde_json::to_string(
        &crate::protocol::HistoryItemPayload::CollaborationModeInstruction {
            mode: crate::agent::mode::ModeKind::Plan,
        },
    )
    .expect("mode payload");
    let communication = crate::protocol::InterAgentCommunication {
        author: "/root/worker".to_string(),
        recipient: "/root".to_string(),
        content: "idle evidence".to_string(),
        trigger_turn: false,
    };
    let mail_history_payload = serde_json::to_string(
        &crate::protocol::HistoryItemPayload::InterAgentCommunication {
            communication: communication.clone(),
        },
    )
    .expect("mail history payload");
    let mail_runtime_payload = serde_json::to_string(
        &crate::protocol::RuntimeEventMsg::InterAgentCommunicationReceived {
            communication: communication.clone(),
        },
    )
    .expect("mail runtime payload");
    let mail_turn_payload =
        serde_json::to_string(&crate::protocol::TurnItemPayload::InterAgentCommunication {
            communication,
        })
        .expect("mail turn payload");

    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('real-user', 'scope-session', 'real-turn', 0, ?1, ?2, 10)",
            (&user_payload, sha256_text(&user_payload)),
        )
        .expect("real history");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES ('scope-session', 'real-turn', 0, 'history_item', 'real-user', 10)",
            [],
        )
        .expect("real append order");
    connection
            .execute(
                "INSERT INTO protocol_turn_items
                 (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
                 VALUES ('real-terminal', 'scope-session', 'real-turn', NULL, 1, ?1, ?2)",
                (&terminal_payload, sha256_text(&terminal_payload)),
            )
            .expect("real terminal");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES ('scope-session', 'real-turn', 1, 'turn_item', 'real-terminal', 11)",
            [],
        )
        .expect("terminal append order");
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('mode-history', 'scope-session', 'mode-pseudo-turn', 0, ?1, ?2, 20)",
            (&mode_payload, sha256_text(&mode_payload)),
        )
        .expect("mode pseudo history");
    connection
            .execute(
                "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES ('scope-session', 'mode-pseudo-turn', 0, 'history_item', 'mode-history', 20)",
                [],
            )
            .expect("mode append order");
    connection
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES ('mail-runtime', 'scope-session', 'mail-pseudo-turn', 0, ?1, ?2, 30)",
            (&mail_runtime_payload, sha256_text(&mail_runtime_payload)),
        )
        .expect("mail runtime");
    connection
            .execute(
                "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES ('scope-session', 'mail-pseudo-turn', 0, 'runtime_event', 'mail-runtime', 30)",
                [],
            )
            .expect("mail runtime append order");
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('mail-history', 'scope-session', 'mail-pseudo-turn', 0, ?1, ?2, 31)",
            (&mail_history_payload, sha256_text(&mail_history_payload)),
        )
        .expect("mail history");
    connection
            .execute(
                "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES ('scope-session', 'mail-pseudo-turn', 0, 'history_item', 'mail-history', 31)",
                [],
            )
            .expect("mail history append order");
    connection
            .execute(
                "INSERT INTO protocol_turn_items
                 (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
                 VALUES ('mail-turn', 'scope-session', 'mail-pseudo-turn', 'mail-history', 0, ?1, ?2)",
                (&mail_turn_payload, sha256_text(&mail_turn_payload)),
            )
            .expect("mail turn projection");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES ('scope-session', 'mail-pseudo-turn', 0, 'turn_item', 'mail-turn', 31)",
            [],
        )
        .expect("mail turn append order");
    connection
        .execute_batch(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
                 VALUES
                 ('scope-session', 'real-turn', 2),
                 ('scope-session', 'mode-pseudo-turn', 1),
                 ('scope-session', 'mail-pseudo-turn', 1);",
        )
        .expect("sequence allocators");

    run(&connection).expect("V42 scope cutover");

    let rows = connection
        .prepare(
            "SELECT history.id, history.scope_kind, history.turn_id, history.sequence_no
                 FROM protocol_history_items AS history
                 INNER JOIN protocol_item_append_order AS append_order
                   ON append_order.source_kind = 'history_item'
                  AND append_order.source_id = history.id
                 ORDER BY append_order.append_position ASC",
        )
        .expect("scoped history query")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .expect("scoped history rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("scoped history");
    assert_eq!(
        rows,
        vec![
            (
                "real-user".to_string(),
                "turn".to_string(),
                Some("real-turn".to_string()),
                0,
            ),
            ("mode-history".to_string(), "session".to_string(), None, 0),
            ("mail-history".to_string(), "session".to_string(), None, 1),
        ]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_runtime_events WHERE turn_id = 'mail-pseudo-turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("retired mail runtime count"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_turn_items WHERE turn_id = 'mail-pseudo-turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("retired mail turn count"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_item_append_order
                     WHERE source_id IN ('mail-runtime', 'mail-turn')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("retired known mail projection order"),
        0,
        "only the allow-listed legacy mail projections are deleted during cutover"
    );
    let allocators = connection
        .prepare(
            "SELECT turn_id FROM protocol_turn_sequence_allocators
                 WHERE session_id = 'scope-session' ORDER BY turn_id",
        )
        .expect("allocator query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("allocator rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("allocators");
    assert_eq!(allocators, vec!["real-turn"]);
    assert_eq!(foreign_keys_setting(&connection), 1);
    assert!(foreign_key_violations(&connection).is_empty());
    validate_canonical_protocol_storage(&connection).expect("full V42 audit");
    assert!(
        connection
            .execute(
                "INSERT INTO protocol_history_items
                     (id, session_id, scope_kind, turn_id, sequence_no, payload_json,
                      payload_sha256, created_at_ms)
                     VALUES ('ambiguous-session', 'scope-session', 'session', 'invented-turn',
                             99, ?1, ?2, 99)",
                (&mail_history_payload, sha256_text(&mail_history_payload)),
            )
            .is_err(),
        "session scope must reject a non-null turn identity"
    );
    assert!(
        connection
            .execute(
                "INSERT INTO protocol_history_items
                     (id, session_id, scope_kind, turn_id, sequence_no, payload_json,
                      payload_sha256, created_at_ms)
                     VALUES ('ambiguous-turn', 'scope-session', 'turn', NULL,
                             99, ?1, ?2, 99)",
                (&user_payload, sha256_text(&user_payload)),
            )
            .is_err(),
        "turn scope must reject a null turn identity"
    );
}

#[test]
fn v42_fails_closed_on_unknown_pseudo_turn_projections() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v40(&connection);
    run_indexed_collaboration_mode_lookup(&connection).expect("V41 schema");
    let mode_payload = serde_json::to_string(
        &crate::protocol::HistoryItemPayload::CollaborationModeInstruction {
            mode: crate::agent::mode::ModeKind::Plan,
        },
    )
    .expect("mode payload");
    let warning_payload = serde_json::to_string(&crate::protocol::RuntimeEventMsg::Warning {
        message: "unexpected projection".to_string(),
    })
    .expect("warning payload");
    let warning_turn_payload = serde_json::to_string(&crate::protocol::TurnItemPayload::Warning {
        message: "unexpected projection".to_string(),
    })
    .expect("warning turn payload");
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('mode-history', 'session', 'mode-turn', 0, ?1, ?2, 1)",
            (&mode_payload, sha256_text(&mode_payload)),
        )
        .expect("mode history");
    connection
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES ('unexpected-runtime', 'session', 'mode-turn', 0, ?1, ?2, 1)",
            (&warning_payload, sha256_text(&warning_payload)),
        )
        .expect("unexpected runtime");
    connection
            .execute(
                "INSERT INTO protocol_turn_items
                 (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
                 VALUES ('unexpected-turn-item', 'session', 'mode-turn', NULL, 0, ?1, ?2)",
                (
                    &warning_turn_payload,
                    sha256_text(&warning_turn_payload),
                ),
            )
            .expect("unexpected turn item");
    connection
        .execute_batch(
            "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES
                 ('session', 'mode-turn', 0, 'history_item', 'mode-history', 1),
                 ('session', 'mode-turn', 0, 'runtime_event', 'unexpected-runtime', 1),
                 ('session', 'mode-turn', 0, 'turn_item', 'unexpected-turn-item', 1);",
        )
        .expect("append order");

    let error = run(&connection).expect_err("unknown mode projection must fail closed");
    assert!(
        error
            .to_string()
            .contains("unexpected mail, runtime, or turn")
    );
    assert!(
        !schema_migration_applied(&connection, TYPED_HISTORY_SCOPE_VERSION).expect("no V42 marker")
    );
    assert_eq!(foreign_keys_setting(&connection), 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_history_items
                     WHERE session_id = 'session' AND turn_id = 'mode-turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("mode history remains after rollback"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_runtime_events
                     WHERE session_id = 'session' AND turn_id = 'mode-turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("mode runtime remains after rollback"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_turn_items
                     WHERE session_id = 'session' AND turn_id = 'mode-turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("mode turn item remains after rollback"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_item_append_order
                     WHERE session_id = 'session' AND turn_id = 'mode-turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("mode append order remains after rollback"),
        3
    );

    let mail_connection = Connection::open_in_memory().expect("mail database");
    run_through_v40(&mail_connection);
    run_indexed_collaboration_mode_lookup(&mail_connection).expect("mail V41 schema");
    let communication = crate::protocol::InterAgentCommunication {
        author: "/root/worker".to_string(),
        recipient: "/root".to_string(),
        content: "idle evidence".to_string(),
        trigger_turn: false,
    };
    let mail_payload = serde_json::to_string(
        &crate::protocol::HistoryItemPayload::InterAgentCommunication { communication },
    )
    .expect("mail payload");
    mail_connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('mail-history', 'session', 'mail-turn', 0, ?1, ?2, 1)",
            (&mail_payload, sha256_text(&mail_payload)),
        )
        .expect("mail history");
    mail_connection
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES ('unknown-mail-runtime', 'session', 'mail-turn', 0, ?1, ?2, 1)",
            (&warning_payload, sha256_text(&warning_payload)),
        )
        .expect("unknown mail runtime");
    mail_connection
        .execute_batch(
            "INSERT INTO protocol_item_append_order
                 (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                 VALUES
                 ('session', 'mail-turn', 0, 'history_item', 'mail-history', 1),
                 ('session', 'mail-turn', 0, 'runtime_event', 'unknown-mail-runtime', 1);",
        )
        .expect("mail append order");
    let error = run(&mail_connection).expect_err("unknown mail projection must fail closed");
    assert!(error.to_string().contains("terminal-less mail-only"));
    assert!(
        !schema_migration_applied(&mail_connection, TYPED_HISTORY_SCOPE_VERSION)
            .expect("no mail V42 marker")
    );
    assert_eq!(foreign_keys_setting(&mail_connection), 1);
}

#[test]
fn v39_rewrites_terminal_outcomes_and_deletes_retired_durable_rows() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v38(&connection);
    connection
            .execute_batch(
                r#"INSERT INTO projects
                   (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                   VALUES ('project', 'C:/workspace', 'workspace', 'none', 1, 1);
                   INSERT INTO sessions
                   (id, project_id, title, status, cwd_path, model_name, base_url,
                    created_at_ms, updated_at_ms, completed_at_ms)
                   VALUES ('session', 'project', 'terminal migration fixture', 'idle',
                           'C:/workspace', 'model', 'http://localhost', 1, 1, NULL);

                   INSERT INTO protocol_runtime_events
                   (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                   VALUES
                   ('runtime-completed', 'session', 'completed-turn', 1,
                    '{"kind":"turn_terminal","terminal":{"status":"completed","finish_reason":"stop","interruption_cause":null,"summary":"canonical assistant text","tool_call_count":1,"failed_tool_count":0,"change_count":2,"metrics":{}}}',
                    'old-completed-hash', 1),
                   ('runtime-interrupted', 'session', 'interrupted-turn', 1,
                    '{"kind":"turn_terminal","terminal":{"status":"interrupted","finish_reason":"cancelled","interruption_cause":"user_stop","summary":"custom text is not the cause owner","tool_call_count":0,"failed_tool_count":0,"change_count":0,"metrics":{}}}',
                    'old-interrupted-hash', 2),
                   ('runtime-failed', 'session', 'failed-turn', 1,
                    '{"kind":"turn_terminal","terminal":{"status":"failed","finish_reason":"error","interruption_cause":null,"summary":"provider failed","tool_call_count":0,"failed_tool_count":0,"change_count":0,"metrics":{}}}',
                    'old-failed-hash', 3),
                   ('runtime-thread', 'session', 'dead-turn-1', 1,
                    '{"kind":"thread_configured","model":"model","base_url":"http://localhost"}',
                    'dead', 4),
                   ('runtime-text', 'session', 'dead-turn-2', 1,
                    '{"kind":"assistant_text_delta","response_id":"response","delta":"partial"}',
                    'dead', 5),
                   ('runtime-reasoning', 'session', 'dead-turn-3', 1,
                    '{"kind":"reasoning_summary_delta","response_id":"response","delta":"partial"}',
                    'dead', 6),
                   ('runtime-history', 'session', 'dead-turn-4', 1,
                    '{"kind":"history_item_recorded","item_id":"history"}',
                    'dead', 7),
                   ('runtime-retry', 'session', 'retry-turn', 1,
                    '{"kind":"retry_scheduled","attempt":2,"message":"retry","next_retry_at_ms":99}',
                    'dead', 8);

                   INSERT INTO protocol_history_items
                   (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                   VALUES ('history-retry', 'session', 'retry-turn', 1,
                           '{"kind":"retry_decision","attempt":2,"message":"retry","next_retry_at_ms":99}',
                           'dead', 8);

                   INSERT INTO protocol_turn_items
                   (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
                   VALUES
                   ('turn-completed', 'session', 'completed-turn', NULL, 1,
                    '{"kind":"terminal","status":"completed","summary":"canonical assistant text","cause":null}',
                    'old-completed-turn-hash'),
                   ('turn-interrupted', 'session', 'interrupted-turn', NULL, 1,
                    '{"kind":"terminal","status":"interrupted","summary":"ignored projection text","cause":"tree_stopped"}',
                    'old-interrupted-turn-hash'),
                   ('turn-failed', 'session', 'failed-turn', NULL, 1,
                    '{"kind":"terminal","status":"failed","summary":"different projection error","cause":null}',
                    'old-failed-turn-hash'),
                   ('turn-retry', 'session', 'retry-turn', 'history-retry', 1,
                    '{"kind":"warning","message":"retry"}', 'dead');

                   INSERT INTO protocol_item_append_order
                   (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                   SELECT session_id, turn_id, sequence_no, 'runtime_event', id, created_at_ms
                   FROM protocol_runtime_events;
                   INSERT INTO protocol_item_append_order
                   (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                   VALUES
                   ('session', 'retry-turn', 1, 'history_item', 'history-retry', 8),
                   ('session', 'completed-turn', 1, 'turn_item', 'turn-completed', 1),
                   ('session', 'interrupted-turn', 1, 'turn_item', 'turn-interrupted', 2),
                   ('session', 'failed-turn', 1, 'turn_item', 'turn-failed', 3),
                   ('session', 'retry-turn', 1, 'turn_item', 'turn-retry', 8);"#,
            )
            .expect("V38 terminal fixtures");

    run(&connection).expect("V39 cutover");
    run(&connection).expect("idempotent V39 validation");

    assert!(
        schema_migration_applied(&connection, TERMINAL_OUTCOME_CUTOVER_VERSION)
            .expect("V39 marker")
    );
    assert_eq!(
        retired_durable_protocol_row_count(&connection).expect("retired rows"),
        0
    );
    assert_eq!(
        orphaned_protocol_append_order_count(&connection).expect("append order"),
        0
    );
    for (id, expected_kind) in [
        ("runtime-completed", "completed"),
        ("runtime-interrupted", "interrupted"),
        ("runtime-failed", "failed"),
    ] {
        let (json, hash) = connection
            .query_row(
                "SELECT msg_json, payload_sha256 FROM protocol_runtime_events WHERE id = ?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("migrated runtime terminal");
        let value: serde_json::Value = serde_json::from_str(&json).expect("terminal JSON");
        let terminal = value.get("terminal").expect("terminal object");
        assert_eq!(
            terminal
                .pointer("/outcome/kind")
                .and_then(serde_json::Value::as_str),
            Some(expected_kind)
        );
        assert!(terminal.get("status").is_none());
        assert!(terminal.get("summary").is_none());
        assert_eq!(hash, sha256_text(&json));
    }
    let completed_turn_json = connection
        .query_row(
            "SELECT payload_json FROM protocol_turn_items WHERE id = 'turn-completed'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("completed turn projection");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&completed_turn_json).expect("turn JSON"),
        serde_json::json!({"kind":"terminal","outcome":{"kind":"completed"}})
    );
    let interrupted_turn_json = connection
        .query_row(
            "SELECT payload_json FROM protocol_turn_items WHERE id = 'turn-interrupted'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("interrupted turn projection");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&interrupted_turn_json)
            .expect("turn JSON")
            .pointer("/outcome/cause")
            .and_then(serde_json::Value::as_str),
        Some("user_stop"),
        "the runtime outcome owns the same-sequence turn projection"
    );
}

#[test]
fn v40_cleanup_reaches_v47_recursive_lineage_without_deleting_detached_sessions() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v39(&connection);
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('flat-project', 'C:/flat', 'flat', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES ('flat-root', 'flat-project', 'root', 'idle', 'C:/flat', 'model',
                         'http://localhost', 1, 1, NULL);",
        )
        .expect("root fixture");
    for index in 0..=255 {
        let session_id = format!("flat-child-{index:03}");
        let task_name = format!("child_{index:03}");
        let agent_path = format!("/root/{task_name}");
        connection
            .execute(
                "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES (?1, 'flat-project', ?2, 'idle', 'C:/flat', 'model',
                             'http://localhost', ?3, ?3, NULL)",
                params![session_id, task_name, i64::from(index) + 2],
            )
            .expect("direct child session");
        connection
            .execute(
                "INSERT INTO session_spawn_edges
                     (root_session_id, parent_session_id, child_session_id,
                      agent_path, task_name, created_at_ms)
                     VALUES ('flat-root', 'flat-root', ?1, ?2, ?3, ?4)",
                params![session_id, agent_path, task_name, i64::from(index) + 2],
            )
            .expect("legacy direct edge");
    }
    connection
        .execute_batch(
            "INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES ('nested-session', 'flat-project', 'nested', 'idle', 'C:/flat', 'model',
                         'http://localhost', 999, 999, NULL);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, created_at_ms)
                 VALUES ('flat-root', 'flat-child-000', 'nested-session',
                         '/root/child_000/nested', 'nested', 999);",
        )
        .expect("legacy nested edge");

    run(&connection).expect("V40 cleanup through current storage");

    assert!(
        schema_migration_applied(&connection, FLATTEN_SESSION_SPAWN_EDGES_VERSION)
            .expect("V40 marker")
    );
    assert!(
        schema_migration_applied(&connection, RECURSIVE_SESSION_SPAWN_EDGES_VERSION)
            .expect("V47 marker")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM session_spawn_edges WHERE root_session_id = 'flat-root'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("retained edge count"),
        255
    );
    for detached_session in ["flat-child-255", "nested-session"] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    [detached_session],
                    |row| row.get::<_, i64>(0),
                )
                .expect("detached session"),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM session_spawn_edges WHERE child_session_id = ?1",
                    [detached_session],
                    |row| row.get::<_, i64>(0),
                )
                .expect("discarded edge"),
            0
        );
    }

    connection
        .execute_batch(
            "INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('other-root', 'flat-project', 'other root', 'idle', 'C:/flat', 'model',
                   'http://localhost', 2000, 2000, NULL),
                 ('other-parent', 'flat-project', 'other parent', 'idle', 'C:/flat', 'model',
                   'http://localhost', 2001, 2001, NULL),
                 ('other-child', 'flat-project', 'other child', 'idle', 'C:/flat', 'model',
                   'http://localhost', 2002, 2002, NULL);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('other-root', 'other-root', 'other-parent',
                         '/root/other_parent', 'other_parent', 1, 2001);",
        )
        .expect("post-cutover sessions");
    connection
        .execute(
            "INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('other-root', 'other-parent', 'other-child',
                         '/root/other_parent/other_child', 'other_child', 2, 2002)",
            [],
        )
        .expect("V47 must accept canonical nested lineage");
    run(&connection).expect("idempotent current validation with nested lineage");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM session_spawn_edges
                     WHERE root_session_id = 'other-root'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("recursive edge count"),
        2
    );
    assert!(
        connection
            .execute(
                "INSERT INTO session_spawn_edges
                     (root_session_id, parent_session_id, child_session_id,
                      agent_path, task_name, spawn_order, created_at_ms)
                     VALUES ('flat-root', 'flat-root', 'flat-child-255',
                             '/root/child_255', 'child_255', 256, 3000)",
                [],
            )
            .is_err(),
        "the per-root capacity trigger must reject a 256th child"
    );
}

#[test]
fn v40_fails_closed_instead_of_detaching_an_active_tree() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v39(&connection);
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('active-project', 'C:/active', 'active', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms, active_run_id,
                  active_turn_id, active_run_lease_expires_at_ms)
                 VALUES
                 ('active-root', 'active-project', 'root', 'running', 'C:/active', 'model',
                  'http://localhost', 1, 1, NULL, 'run', 'turn', 999),
                 ('active-parent', 'active-project', 'parent', 'idle', 'C:/active', 'model',
                  'http://localhost', 2, 2, NULL, NULL, NULL, NULL),
                 ('active-child', 'active-project', 'child', 'idle', 'C:/active', 'model',
                  'http://localhost', 3, 3, NULL, NULL, NULL, NULL);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, created_at_ms)
                 VALUES ('active-root', 'active-parent', 'active-child',
                         '/root/parent/child', 'child', 3);",
        )
        .expect("active nested fixture");
    let edge_before = text_snapshot(
        &connection,
        "SELECT json_group_array(json_array(root_session_id, parent_session_id,
                                                child_session_id, agent_path, task_name,
                                                created_at_ms))
             FROM (SELECT * FROM session_spawn_edges ORDER BY child_session_id)",
    );

    let error = run_flatten_session_spawn_edges(&connection)
        .expect_err("active nested lineage must not be detached");

    assert!(error.to_string().contains("retains active run state"));
    assert_eq!(
        text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(root_session_id, parent_session_id,
                                                    child_session_id, agent_path, task_name,
                                                    created_at_ms))
                 FROM (SELECT * FROM session_spawn_edges ORDER BY child_session_id)",
        ),
        edge_before
    );
    assert!(connection.is_autocommit());
    assert!(
        !schema_migration_applied(&connection, FLATTEN_SESSION_SPAWN_EDGES_VERSION)
            .expect("no V40 marker")
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v39_fails_closed_when_interruption_cause_cannot_be_recovered() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v38(&connection);
    let invalid_json = r#"{"kind":"turn_terminal","terminal":{"status":"interrupted","finish_reason":"cancelled","summary":"ambiguous custom interruption","tool_call_count":0,"failed_tool_count":0,"change_count":0,"metrics":{}}}"#;
    connection
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES ('invalid-terminal', 'session', 'turn', 1, ?1, ?2, 1)",
            (invalid_json, sha256_text(invalid_json)),
        )
        .expect("invalid legacy terminal fixture");

    let error = run(&connection).expect_err("ambiguous cause must fail closed");
    assert!(
        error
            .to_string()
            .contains("uniquely recognized legacy summary")
    );
    assert!(connection.is_autocommit());
    assert!(
        !schema_migration_applied(&connection, TERMINAL_OUTCOME_CUTOVER_VERSION)
            .expect("no V39 marker")
    );
    assert!(
        connection
            .query_row(
                "SELECT msg_json FROM protocol_runtime_events WHERE id = 'invalid-terminal'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("rolled-back terminal")
            .contains("\"status\":\"interrupted\"")
    );
}

#[test]
fn v36_marker_rejects_reintroduced_legacy_reasoning_rows() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    connection
            .execute(
                "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('retired-reasoning', 'session', 'turn', 'turn', 0,
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
    let payload: serde_json::Value = serde_json::from_str(&payload_json).expect("canonical JSON");
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
        schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION).expect("V37 marker")
    );
}

#[test]
fn v37_recovers_unique_response_lineage_without_changing_other_turn_evidence() {
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
    connection
        .execute(
            "UPDATE sessions SET active_turn_id = 'legacy-turn' WHERE id = 'tool-session'",
            [],
        )
        .expect("active legacy turn");

    let unchanged_snapshots = [
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms))
                 FROM (SELECT * FROM protocol_runtime_events ORDER BY session_id, turn_id, sequence_no, id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms))
                 FROM (SELECT * FROM protocol_history_items WHERE id <> 'legacy-tool-history'
                       ORDER BY session_id, turn_id, sequence_no, id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256))
                 FROM (SELECT * FROM protocol_turn_items ORDER BY session_id, turn_id, sequence_no, id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(append_position, session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms))
                 FROM (SELECT * FROM protocol_item_append_order ORDER BY append_position)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(session_id, turn_id, next_sequence_no))
                 FROM (SELECT * FROM protocol_turn_sequence_allocators ORDER BY session_id, turn_id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms))
                 FROM (SELECT * FROM tool_calls ORDER BY id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, tool_call_id, change_kind, path_before, path_after, before_sha256, after_sha256, diff_text, summary_text, created_at_ms))
                 FROM (SELECT * FROM file_changes ORDER BY id)",
            ),
        ];

    run_raw_tool_call_history_migration(&connection).expect("recoverable V37 cutover");

    let snapshots_after = [
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms))
                 FROM (SELECT * FROM protocol_runtime_events ORDER BY session_id, turn_id, sequence_no, id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms))
                 FROM (SELECT * FROM protocol_history_items WHERE id <> 'legacy-tool-history'
                       ORDER BY session_id, turn_id, sequence_no, id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256))
                 FROM (SELECT * FROM protocol_turn_items ORDER BY session_id, turn_id, sequence_no, id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(append_position, session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms))
                 FROM (SELECT * FROM protocol_item_append_order ORDER BY append_position)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(session_id, turn_id, next_sequence_no))
                 FROM (SELECT * FROM protocol_turn_sequence_allocators ORDER BY session_id, turn_id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms))
                 FROM (SELECT * FROM tool_calls ORDER BY id)",
            ),
            text_snapshot(
                &connection,
                "SELECT json_group_array(json_array(id, tool_call_id, change_kind, path_before, path_after, before_sha256, after_sha256, diff_text, summary_text, created_at_ms))
                 FROM (SELECT * FROM file_changes ORDER BY id)",
            ),
        ];
    assert_eq!(snapshots_after, unchanged_snapshots);
    assert_eq!(
        connection
            .query_row(
                "SELECT active_turn_id FROM sessions WHERE id = 'tool-session'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("active turn after V37")
            .as_deref(),
        Some("legacy-turn")
    );
    let (payload_json, payload_sha256) = connection
        .query_row(
            "SELECT payload_json, payload_sha256 FROM protocol_history_items
                 WHERE id = 'legacy-tool-history'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("recovered tool call");
    let payload: serde_json::Value =
        serde_json::from_str(&payload_json).expect("canonical tool-call JSON");
    assert_eq!(payload["response_id"], "old");
    assert_eq!(payload["tool_name"], "read");
    assert_eq!(payload["arguments_json"], r#"{"path":"README.md"}"#);
    assert_eq!(payload_sha256, sha256_text(&payload_json));
    assert!(
        schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION).expect("V37 marker")
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v37_fails_closed_when_missing_response_lineage_is_not_uniquely_recoverable() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v36(&connection).expect("schema through V36");
    insert_canonical_tool_call_parent_rows(&connection);
    connection
            .execute_batch(
                r#"UPDATE sessions
                   SET active_turn_id = 'unresolved-turn'
                   WHERE id = 'tool-session';
                   INSERT INTO protocol_history_items
                   (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                   VALUES
                   ('unresolved-tool', 'tool-session', 'unresolved-turn', 0,
                    '{"kind":"tool_call","call_id":"unresolved-call","tool":"read","arguments":{"path":"README.md"}}',
                    'unresolved-tool-hash', 3),
                   ('unresolved-output', 'tool-session', 'unresolved-turn', 1,
                    '{"kind":"tool_output","call_id":"unresolved-call","status":"completed","title":"read","output_text":"old","metadata":null}',
                    'unresolved-output-hash', 4);
                   INSERT INTO protocol_runtime_events
                   (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                   VALUES ('unresolved-runtime', 'tool-session', 'unresolved-turn', 0,
                           '{"kind":"warning","message":"preserve me"}', 'runtime-hash', 3);
                   INSERT INTO protocol_turn_items
                   (id, session_id, turn_id, source_item_id, sequence_no, payload_json, payload_sha256)
                   VALUES ('unresolved-turn-item', 'tool-session', 'unresolved-turn',
                           'unresolved-tool', 0,
                           '{"kind":"tool_status","call_id":"unresolved-call","tool":"read","status":"completed","title":"read","summary":"old"}',
                           'turn-hash');
                   INSERT INTO protocol_item_append_order
                   (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
                   VALUES
                   ('tool-session', 'unresolved-turn', 0, 'runtime_event', 'unresolved-runtime', 3),
                   ('tool-session', 'unresolved-turn', 1, 'history_item', 'unresolved-tool', 3),
                   ('tool-session', 'unresolved-turn', 2, 'history_item', 'unresolved-output', 4),
                   ('tool-session', 'unresolved-turn', 3, 'turn_item', 'unresolved-turn-item', 4);
                   INSERT INTO protocol_turn_sequence_allocators
                   (session_id, turn_id, next_sequence_no)
                   VALUES ('tool-session', 'unresolved-turn', 4);
                   INSERT INTO tool_calls
                   (id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms)
                   VALUES ('unresolved-call', 'unresolved-tool', 'completed', 'C:/old.txt', 3, 4);
                   INSERT INTO file_changes
                   (id, tool_call_id, change_kind, path_before, path_after, before_sha256,
                    after_sha256, diff_text, summary_text, created_at_ms)
                   VALUES ('unresolved-change', 'unresolved-call', 'update', 'README.md',
                           'README.md', 'before', 'after', 'diff', 'summary', 4);"#,
            )
            .expect("unresolved V37 fixture");
    let before = v37_byte_order_snapshot(&connection);

    let error = run_raw_tool_call_history_migration(&connection)
        .expect_err("missing response lineage must fail closed");

    assert!(
        error
            .to_string()
            .contains("no uniquely recoverable assistant response lineage")
    );
    assert_eq!(v37_byte_order_snapshot(&connection), before);
    assert!(connection.is_autocommit());
    assert!(
        !schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
            .expect("no V37 marker")
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
fn full_canonical_validation_accepts_invalid_provider_json_text_but_rejects_old_keys() {
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
                 (id, session_id, scope_kind, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
                 VALUES ('raw-tool-history', 'tool-session', 'turn', 'turn', 0, ?1, ?2, 3)",
                (&raw_payload, sha256_text(&raw_payload)),
            )
            .expect("raw tool-call fixture");
    validate_canonical_protocol_storage(&connection)
        .expect("raw invalid provider arguments remain valid history");

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
    let error = validate_canonical_protocol_storage(&connection)
        .expect_err("old tool key must fail full canonical validation");
    assert!(error.to_string().contains("unexpected fields: tool"));
}

#[test]
fn released_v31_messages_tools_and_active_turn_survive_the_current_cutover() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v30(&connection).expect("released v30 schema");
    run_tool_call_declined_cancelled_status_migration(&connection).expect("released v31 schema");
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
        schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION).expect("V37 marker")
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
        assert!(
                connection
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM {table} WHERE session_id = 'tool-session' AND turn_id = 'legacy-turn'"
                        ),
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or_else(|error| panic!("preserved legacy {table}: {error}"))
                    > 0,
                "legacy table={table}"
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
                .unwrap_or_else(|error| panic!("preserved {table}: {error}")),
            2,
            "sidecar table={table}"
        );
    }
    for call_id in ["matched", "unmatched"] {
        let payload = connection
            .query_row(
                "SELECT payload_json FROM protocol_history_items
                     WHERE json_valid(payload_json)
                       AND json_extract(payload_json, '$.kind') = 'tool_call'
                       AND json_extract(payload_json, '$.call_id') = ?1",
                [call_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_else(|error| panic!("canonical call {call_id}: {error}"));
        let value: serde_json::Value = serde_json::from_str(&payload).expect("canonical call JSON");
        assert!(
            value["response_id"]
                .as_str()
                .is_some_and(|response_id| !response_id.is_empty()),
            "call={call_id}"
        );
        assert_eq!(value["tool_name"], "shell", "call={call_id}");
        assert_eq!(
            value["arguments_json"],
            format!(r#"{{"id":"{call_id}"}}"#),
            "call={call_id}"
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
    run_tool_call_declined_cancelled_status_migration(&connection).expect("released v31 schema");
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

    run(&connection).expect("v34 through v45 upgrade");
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
        schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION).expect("v37 marker")
    );
    assert!(
        schema_migration_applied(&connection, REMOVE_AUTO_REVIEW_ACCESS_MODE_VERSION)
            .expect("v38 marker")
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
            "default".to_string(),
            "{\"temperature\":0.2}".to_string(),
            2,
            3,
        )
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn fresh_v45_schema_accepts_exactly_the_three_current_access_modes() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");

    run(&connection).expect("fresh current schema");

    assert!(
        schema_migration_applied(&connection, REMOVE_AUTO_REVIEW_ACCESS_MODE_VERSION)
            .expect("v38 marker")
    );
    assert!(
        schema_migration_applied(&connection, RESTORE_AUTO_REVIEW_ACCESS_MODE_VERSION)
            .expect("v45 marker")
    );
    assert!(
        table_has_exact_access_mode_domain(&connection, "sessions", SESSION_ACCESS_MODE_DOMAIN,)
            .expect("access mode domain")
    );
    connection
        .execute(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('project-v38', 'C:/workspace', 'workspace', 'none', 1, 1)",
            [],
        )
        .expect("project");
    for (id, access_mode) in [
        ("default-session", "default"),
        ("auto-review-session", "auto_review"),
        ("full-session", "full_access"),
    ] {
        connection
            .execute(
                "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                      model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES (?1, 'project-v38', ?1, 'idle', 'C:/workspace', 'model',
                             'http://localhost', ?2, '{}', 1, 1, NULL)",
                (id, access_mode),
            )
            .expect("current access mode");
    }
    assert!(
        connection
            .execute(
                "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                      model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES ('unknown-session', 'project-v38', 'unknown', 'idle', 'C:/workspace',
                             'model', 'http://localhost', 'unknown', '{}', 1, 1, NULL)",
                [],
            )
            .is_err()
    );
}

#[test]
fn v37_auto_review_session_upgrades_one_way_to_default() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v36(&connection).expect("v36 schema");
    run_raw_tool_call_history_migration(&connection).expect("v37 schema");
    connection
        .execute_batch(
            r#"INSERT INTO projects
                   (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                   VALUES ('project-v37', 'C:/workspace', 'workspace', 'none', 1, 1);
                   INSERT INTO sessions
                   (id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
                   VALUES ('session-v37', 'project-v37', 'legacy access', 'idle', 'C:/workspace',
                           'model', 'http://localhost', 'auto_review', '{"temperature":0.2}',
                           2, 3, NULL);"#,
        )
        .expect("v37 auto-review fixture");

    run(&connection).expect("v38 through v45 upgrade");
    run(&connection).expect("idempotent v45 upgrade");

    assert_eq!(
        connection
            .query_row(
                "SELECT access_mode FROM sessions WHERE id = 'session-v37'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("migrated access mode"),
        "default"
    );
    assert!(
        schema_migration_applied(&connection, REMOVE_AUTO_REVIEW_ACCESS_MODE_VERSION)
            .expect("v38 marker")
    );
    assert!(
        schema_migration_applied(&connection, RESTORE_AUTO_REVIEW_ACCESS_MODE_VERSION)
            .expect("v45 marker")
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v44_database_reaches_v45_without_changing_existing_access_modes() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v44(&connection);
    connection
        .execute_batch(
            r#"INSERT INTO projects
                   (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                   VALUES ('project-v44', 'C:/workspace', 'workspace', 'none', 1, 1);
                   INSERT INTO sessions
                   (id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
                   VALUES
                   ('default-v44', 'project-v44', 'default', 'idle', 'C:/workspace', 'model',
                    'http://localhost', 'default', '{}', 2, 3, NULL),
                   ('full-v44', 'project-v44', 'full', 'idle', 'C:/workspace', 'model',
                    'http://localhost', 'full_access', '{}', 4, 5, NULL);"#,
        )
        .expect("v44 access fixtures");

    run(&connection).expect("V45 upgrade");
    run(&connection).expect("idempotent V45 upgrade");

    assert!(
        schema_migration_applied(&connection, RESTORE_AUTO_REVIEW_ACCESS_MODE_VERSION)
            .expect("V45 marker")
    );
    assert!(
        table_has_exact_access_mode_domain(&connection, "sessions", SESSION_ACCESS_MODE_DOMAIN,)
            .expect("V45 access mode domain")
    );
    let mut statement = connection
        .prepare(
            "SELECT access_mode FROM sessions
                 WHERE id IN ('default-v44', 'full-v44') ORDER BY id",
        )
        .expect("access query");
    let modes = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("access rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("access values");
    assert_eq!(modes, vec!["default", "full_access"]);
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v45_missing_marker_reapplies_without_collapsing_auto_review() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    connection
        .execute_batch(
            r#"INSERT INTO projects
                   (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                   VALUES ('project-v45-retry', 'C:/workspace', 'workspace', 'none', 1, 1);
                   INSERT INTO sessions
                   (id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, created_at_ms, updated_at_ms, completed_at_ms)
                   VALUES ('auto-v45-retry', 'project-v45-retry', 'auto', 'idle', 'C:/workspace',
                           'model', 'http://localhost', 'auto_review', '{}', 2, 3, NULL);
                   DELETE FROM moyai_schema_migrations WHERE version = 45;"#,
        )
        .expect("missing-marker fixture");

    run(&connection).expect("reapply V45");

    assert_eq!(
        connection
            .query_row(
                "SELECT access_mode FROM sessions WHERE id = 'auto-v45-retry'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("preserved auto-review mode"),
        "auto_review"
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v45_marker_requires_the_exact_three_mode_domain() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    run_remove_auto_review_access_mode(&connection).expect("replace with stale V38 domain");

    let error = run(&connection).expect_err("V45 marker must validate the access mode domain");
    assert!(error.to_string().contains("V45 access mode marker"));
}

#[test]
fn v46_migrates_v1_compaction_payload_and_is_idempotent() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);
    let replaced_id = crate::protocol::HistoryItemId::new().to_string();
    insert_v46_replacement(&connection, &replaced_id, 0);
    let legacy_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "legacy checkpoint",
        "replacement_item_ids": [replaced_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        "legacy-compaction",
        1,
        &legacy_payload,
        &sha256_text(&legacy_payload),
    );

    run(&connection).expect("V46 forward migration");
    let (migrated_json, migrated_hash) = connection
        .query_row(
            "SELECT payload_json, payload_sha256
                 FROM protocol_history_items WHERE id = 'legacy-compaction'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("migrated compaction");
    let migrated: serde_json::Value =
        serde_json::from_str(&migrated_json).expect("current compaction JSON");
    assert_eq!(migrated["layout"], "legacy_prefix");
    assert_eq!(migrated["preserved_user_messages"], serde_json::json!([]));
    assert_eq!(migrated["summary"], "legacy checkpoint");
    assert_eq!(
        migrated["replacement_item_ids"],
        serde_json::json!([replaced_id])
    );
    assert_eq!(migrated_hash, sha256_text(&migrated_json));
    let decoded = serde_json::from_str::<crate::protocol::HistoryItemPayload>(&migrated_json)
        .expect("typed current compaction");
    assert!(matches!(
        decoded,
        crate::protocol::HistoryItemPayload::Compaction {
            layout: crate::protocol::CompactionLayout::LegacyPrefix,
            preserved_user_messages,
            ..
        } if preserved_user_messages.is_empty()
    ));
    assert!(
        schema_migration_applied(&connection, CODEX_COMPACTION_CHECKPOINT_VERSION)
            .expect("V46 marker")
    );

    run(&connection).expect("idempotent V46 migration");
    let second = connection
        .query_row(
            "SELECT payload_json, payload_sha256
                 FROM protocol_history_items WHERE id = 'legacy-compaction'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("idempotent compaction");
    assert_eq!(second, (migrated_json, migrated_hash));
}

#[test]
fn v46_recovers_real_user_anchors_through_nested_legacy_compactions() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);

    let user_id = crate::protocol::HistoryItemId::new().to_string();
    let user_payload = serde_json::json!({
        "kind": "user_turn",
        "content": [{
            "kind": "text",
            "text": "Use task.md and create all four requested documents."
        }],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &user_id,
        0,
        &user_payload,
        &sha256_text(&user_payload),
    );

    let first_compaction_id = crate::protocol::HistoryItemId::new().to_string();
    let first_compaction_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "The source investigation is complete.",
        "replacement_item_ids": [user_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &first_compaction_id,
        1,
        &first_compaction_payload,
        &sha256_text(&first_compaction_payload),
    );

    let second_compaction_id = crate::protocol::HistoryItemId::new().to_string();
    let second_compaction_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "Continue from the completed investigation.",
        "replacement_item_ids": [first_compaction_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &second_compaction_id,
        2,
        &second_compaction_payload,
        &sha256_text(&second_compaction_payload),
    );

    run(&connection).expect("V46 nested legacy recovery");

    let migrated_json = connection
        .query_row(
            "SELECT payload_json FROM protocol_history_items WHERE id = ?1",
            [&second_compaction_id],
            |row| row.get::<_, String>(0),
        )
        .expect("outer migrated compaction");
    let migrated: serde_json::Value =
        serde_json::from_str(&migrated_json).expect("outer current compaction JSON");
    assert_eq!(migrated["layout"], "user_anchored_checkpoint");
    assert_eq!(
        migrated["preserved_user_messages"],
        serde_json::json!(["Use task.md and create all four requested documents."])
    );
    assert!(
        !migrated["summary"]
            .as_str()
            .expect("summary")
            .contains("four requested documents")
    );
    assert_eq!(
        migrated["replacement_item_ids"],
        serde_json::json!([first_compaction_id])
    );
}

#[test]
fn v46_preserves_effective_replacement_order_when_late_input_precedes_a_checkpoint() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);

    let original_user_id = crate::protocol::HistoryItemId::new().to_string();
    let late_user_id = crate::protocol::HistoryItemId::new().to_string();
    let first_compaction_id = crate::protocol::HistoryItemId::new().to_string();
    let second_compaction_id = crate::protocol::HistoryItemId::new().to_string();
    let original_user_payload = serde_json::json!({
        "kind": "user_turn",
        "content": [{"kind": "text", "text": "original task"}],
    })
    .to_string();
    let late_user_payload = serde_json::json!({
        "kind": "steer_turn",
        "expected_turn_id": "01J00000000000000000000000",
        "content": [{"kind": "text", "text": "late clarification"}],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &original_user_id,
        0,
        &original_user_payload,
        &sha256_text(&original_user_payload),
    );
    insert_v46_compaction(
        &connection,
        &late_user_id,
        1,
        &late_user_payload,
        &sha256_text(&late_user_payload),
    );
    let first_compaction_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "first summary",
        "replacement_item_ids": [original_user_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &first_compaction_id,
        2,
        &first_compaction_payload,
        &sha256_text(&first_compaction_payload),
    );
    let second_compaction_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "second summary",
        "replacement_item_ids": [first_compaction_id, late_user_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &second_compaction_id,
        3,
        &second_compaction_payload,
        &sha256_text(&second_compaction_payload),
    );

    run(&connection).expect("V46 late-input recovery");

    let migrated_json = connection
        .query_row(
            "SELECT payload_json FROM protocol_history_items WHERE id = ?1",
            [&second_compaction_id],
            |row| row.get::<_, String>(0),
        )
        .expect("outer migrated compaction");
    let migrated: serde_json::Value =
        serde_json::from_str(&migrated_json).expect("outer current JSON");
    assert_eq!(
        migrated["preserved_user_messages"],
        serde_json::json!(["original task", "late clarification"])
    );
}

#[test]
fn v46_legacy_recovery_follows_append_order_instead_of_row_id_order() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);

    let user_id = "01J00000000000000000000000";
    let inner_compaction_id = "01J0000000000000000000000Z";
    let later_user_id = "01J00000000000000000000002";
    let outer_compaction_id = "01J00000000000000000000001";
    assert!(outer_compaction_id < inner_compaction_id);

    let original_user = format!("HEAD-{}-TAIL", "x".repeat(100_000));
    let user_payload = serde_json::json!({
        "kind": "user_turn",
        "content": [{"kind": "text", "text": original_user}],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        user_id,
        0,
        &user_payload,
        &sha256_text(&user_payload),
    );
    let inner_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "inner summary",
        "replacement_item_ids": [user_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        inner_compaction_id,
        1,
        &inner_payload,
        &sha256_text(&inner_payload),
    );
    let later_user_payload = serde_json::json!({
        "kind": "user_turn",
        "content": [{"kind": "text", "text": "latest instruction"}],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        later_user_id,
        2,
        &later_user_payload,
        &sha256_text(&later_user_payload),
    );
    let outer_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "outer summary",
        "replacement_item_ids": [inner_compaction_id, later_user_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        outer_compaction_id,
        3,
        &outer_payload,
        &sha256_text(&outer_payload),
    );

    run(&connection).expect("append-ordered V46 recovery");

    let outer_json = connection
        .query_row(
            "SELECT payload_json FROM protocol_history_items WHERE id = ?1",
            [outer_compaction_id],
            |row| row.get::<_, String>(0),
        )
        .expect("outer migrated compaction");
    let outer: serde_json::Value = serde_json::from_str(&outer_json).expect("outer current JSON");
    let anchors = outer["preserved_user_messages"]
        .as_array()
        .expect("outer anchors");
    assert_eq!(anchors.len(), 2);
    assert_eq!(anchors[1], "latest instruction");
    assert_eq!(
        anchors[0]
            .as_str()
            .expect("boundary anchor")
            .matches("compaction checkpoint truncated")
            .count(),
        1
    );
    assert!(
        anchors[0]
            .as_str()
            .expect("boundary anchor")
            .starts_with("HEAD-")
    );
    assert!(
        anchors[0]
            .as_str()
            .expect("boundary anchor")
            .ends_with("-TAIL")
    );
}

#[test]
fn v46_legacy_replacement_cycle_rolls_back_without_a_marker() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);

    let first_id = crate::protocol::HistoryItemId::new().to_string();
    let second_id = crate::protocol::HistoryItemId::new().to_string();
    let first_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "first legacy summary",
        "replacement_item_ids": [second_id],
    })
    .to_string();
    let second_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "second legacy summary",
        "replacement_item_ids": [first_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &first_id,
        0,
        &first_payload,
        &sha256_text(&first_payload),
    );
    insert_v46_compaction(
        &connection,
        &second_id,
        1,
        &second_payload,
        &sha256_text(&second_payload),
    );
    let before = text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(id, payload_json, payload_sha256))
             FROM (SELECT id, payload_json, payload_sha256 FROM protocol_history_items ORDER BY id)",
        );

    let error = run(&connection).expect_err("cyclic V46 lineage must fail");

    assert!(error.to_string().contains("replacement cycle"));
    let after = text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(id, payload_json, payload_sha256))
             FROM (SELECT id, payload_json, payload_sha256 FROM protocol_history_items ORDER BY id)",
        );
    assert_eq!(after, before);
    assert!(
        !schema_migration_applied(&connection, CODEX_COMPACTION_CHECKPOINT_VERSION)
            .expect("rolled-back V46 marker")
    );
}

#[test]
fn v46_full_audit_rejects_a_cycle_between_current_checkpoints() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    insert_v46_compaction_parent(&connection);

    let first_id = crate::protocol::HistoryItemId::new().to_string();
    let second_id = crate::protocol::HistoryItemId::new().to_string();
    let first_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "layout": "user_anchored_checkpoint",
        "preserved_user_messages": ["first task"],
        "summary": "first current summary",
        "replacement_item_ids": [second_id],
    })
    .to_string();
    let second_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "layout": "user_anchored_checkpoint",
        "preserved_user_messages": ["second task"],
        "summary": "second current summary",
        "replacement_item_ids": [first_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        &first_id,
        0,
        &first_payload,
        &sha256_text(&first_payload),
    );
    insert_v46_compaction(
        &connection,
        &second_id,
        1,
        &second_payload,
        &sha256_text(&second_payload),
    );

    let error = validate_canonical_protocol_storage(&connection)
        .expect_err("current checkpoint cycle must fail");

    assert!(
        error
            .to_string()
            .contains("replacement cycle or forward reference")
    );
}

#[test]
fn v46_reapplication_preserves_user_anchored_checkpoint_payload() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);
    let replacement_id = crate::protocol::HistoryItemId::new().to_string();
    insert_v46_replacement(&connection, &replacement_id, 0);
    let checkpoint_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "layout": "user_anchored_checkpoint",
        "preserved_user_messages": ["follow task.md", "keep auto review"],
        "summary": "continue implementation",
        "replacement_item_ids": [replacement_id],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        "current-compaction",
        1,
        &checkpoint_payload,
        &sha256_text(&checkpoint_payload),
    );
    run_codex_compaction_checkpoint(&connection).expect("initial V46 migration");
    let first = connection
        .query_row(
            "SELECT payload_json, payload_sha256
                 FROM protocol_history_items WHERE id = 'current-compaction'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("canonical checkpoint");

    connection
        .execute(
            "DELETE FROM moyai_schema_migrations WHERE version = ?1",
            [CODEX_COMPACTION_CHECKPOINT_VERSION],
        )
        .expect("remove V46 marker");
    run(&connection).expect("reapply V46");
    let second = connection
        .query_row(
            "SELECT payload_json, payload_sha256
                 FROM protocol_history_items WHERE id = 'current-compaction'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("reapplied checkpoint");
    assert_eq!(second, first);
    let value: serde_json::Value =
        serde_json::from_str(&second.0).expect("reapplied checkpoint JSON");
    assert_eq!(
        value["preserved_user_messages"],
        serde_json::json!(["follow task.md", "keep auto review"])
    );
}

#[test]
fn v46_mixed_payload_rolls_back_all_rewrites_and_marker() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);
    let replacement_id = crate::protocol::HistoryItemId::new().to_string();
    insert_v46_replacement(&connection, &replacement_id, 0);
    let valid_legacy = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "first row would migrate",
        "replacement_item_ids": [replacement_id],
    })
    .to_string();
    let mixed = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "layout": "user_anchored_checkpoint",
        "summary": "missing preserved messages",
        "replacement_item_ids": [],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        "a-valid-compaction",
        1,
        &valid_legacy,
        &sha256_text(&valid_legacy),
    );
    insert_v46_compaction(
        &connection,
        "z-mixed-compaction",
        2,
        &mixed,
        &sha256_text(&mixed),
    );
    let before = text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(id, payload_json, payload_sha256))
             FROM (SELECT id, payload_json, payload_sha256 FROM protocol_history_items ORDER BY id)",
        );

    let error = run(&connection).expect_err("mixed V46 payload must fail");
    assert!(error.to_string().contains("mixes legacy and current"));
    let after = text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(id, payload_json, payload_sha256))
             FROM (SELECT id, payload_json, payload_sha256 FROM protocol_history_items ORDER BY id)",
        );
    assert_eq!(after, before);
    assert!(
        !schema_migration_applied(&connection, CODEX_COMPACTION_CHECKPOINT_VERSION)
            .expect("rolled-back V46 marker")
    );
}

#[test]
fn v46_rejects_stale_compaction_hash_without_rewriting_data() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);
    let legacy_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "summary": "hash must be trusted",
        "replacement_item_ids": [],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        "stale-hash-compaction",
        1,
        &legacy_payload,
        "stale",
    );

    let error = run(&connection).expect_err("stale V46 hash must fail");
    assert!(error.to_string().contains("stale payload hash"));
    let stored = connection
        .query_row(
            "SELECT payload_json, payload_sha256
                 FROM protocol_history_items WHERE id = 'stale-hash-compaction'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("unmodified stale row");
    assert_eq!(stored, (legacy_payload, "stale".to_string()));
    assert!(
        !schema_migration_applied(&connection, CODEX_COMPACTION_CHECKPOINT_VERSION)
            .expect("absent V46 marker")
    );
}

#[test]
fn v46_marker_is_fast_path_and_full_audit_checks_payload_contract() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    connection
        .execute(
            "DELETE FROM moyai_schema_migrations WHERE version = ?1",
            [CODEX_COMPACTION_CHECKPOINT_VERSION],
        )
        .expect("remove V46 marker");
    let marker_error =
        run(&connection).expect_err("current V47 fast path must require the V46 marker");
    assert!(
        marker_error
            .to_string()
            .contains("V46 Codex compaction checkpoint marker")
    );
    connection
        .execute_batch(V46_CODEX_COMPACTION_CHECKPOINT)
        .expect("restore V46 marker");

    insert_v46_compaction_parent(&connection);
    let invalid_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "layout": "unknown_layout",
        "preserved_user_messages": [],
        "summary": "invalid current checkpoint",
        "replacement_item_ids": [],
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        "invalid-current-compaction",
        1,
        &invalid_payload,
        &sha256_text(&invalid_payload),
    );

    run(&connection).expect("current fast path must remain marker and schema only");
    let audit_error = validate_canonical_protocol_storage(&connection)
        .expect_err("full audit must inspect V46 payloads");
    assert!(
        audit_error
            .to_string()
            .contains("violates the current payload contract")
    );
}

#[test]
fn v46_marker_requires_the_exact_migration_name() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    connection
        .execute(
            "UPDATE moyai_schema_migrations SET name = 'wrong_v46_name' WHERE version = ?1",
            [CODEX_COMPACTION_CHECKPOINT_VERSION],
        )
        .expect("corrupt V46 marker name");

    let error = run(&connection).expect_err("V46 marker name must fail closed");
    assert!(
        error
            .to_string()
            .contains("name other than `codex_compaction_checkpoint`")
    );
}

#[test]
fn v46_rewrite_pages_only_compaction_rows() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v45(&connection);
    insert_v46_compaction_parent(&connection);

    let large_non_compaction = serde_json::json!({
        "kind": "error",
        "message": "x".repeat(1_000_000),
    })
    .to_string();
    insert_v46_compaction(
        &connection,
        "000-large-non-compaction",
        0,
        &large_non_compaction,
        &sha256_text(&large_non_compaction),
    );
    for index in 0..=COMPACTION_CHECKPOINT_MIGRATION_PAGE_SIZE {
        let payload = serde_json::json!({
            "kind": "compaction",
            "mode": "automatic",
            "summary": format!("legacy checkpoint {index}"),
            "replacement_item_ids": [],
        })
        .to_string();
        insert_v46_compaction(
            &connection,
            &format!("compaction-{index:03}"),
            i64::try_from(index + 1).expect("fixture sequence"),
            &payload,
            &sha256_text(&payload),
        );
    }

    let stats = canonicalize_compaction_checkpoint_history(&connection)
        .expect("bounded V46 canonicalization");

    assert_eq!(stats.pages, 2);
    assert_eq!(stats.rows, COMPACTION_CHECKPOINT_MIGRATION_PAGE_SIZE + 1);
    assert_eq!(
        stats.max_page_rows,
        COMPACTION_CHECKPOINT_MIGRATION_PAGE_SIZE
    );
    let stored_non_compaction = connection
        .query_row(
            "SELECT payload_json FROM protocol_history_items
                 WHERE id = '000-large-non-compaction'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("non-compaction payload");
    assert_eq!(stored_non_compaction, large_non_compaction);
}

#[test]
fn v46_full_audit_rejects_invalid_lineage_and_oversized_anchors() {
    let missing_replacement = Connection::open_in_memory().expect("database");
    run(&missing_replacement).expect("fresh current schema");
    insert_v46_compaction_parent(&missing_replacement);
    let missing_replacement_id = crate::protocol::HistoryItemId::new().to_string();
    let missing_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "layout": "user_anchored_checkpoint",
        "preserved_user_messages": ["retain the user task"],
        "summary": "continue from the checkpoint",
        "replacement_item_ids": [missing_replacement_id],
    })
    .to_string();
    insert_v46_compaction(
        &missing_replacement,
        "missing-lineage-compaction",
        1,
        &missing_payload,
        &sha256_text(&missing_payload),
    );
    let lineage_error = validate_canonical_protocol_storage(&missing_replacement)
        .expect_err("full audit must reject missing replacement lineage");
    assert!(
        lineage_error
            .to_string()
            .contains("missing or cross-session replacement item")
    );

    let oversized_anchor = Connection::open_in_memory().expect("database");
    run(&oversized_anchor).expect("fresh current schema");
    insert_v46_compaction_parent(&oversized_anchor);
    let oversized_payload = serde_json::json!({
        "kind": "compaction",
        "mode": "automatic",
        "layout": "user_anchored_checkpoint",
        "preserved_user_messages": ["x".repeat(80_004)],
        "summary": "continue from the checkpoint",
        "replacement_item_ids": [],
    })
    .to_string();
    insert_v46_compaction(
        &oversized_anchor,
        "oversized-anchor-compaction",
        1,
        &oversized_payload,
        &sha256_text(&oversized_payload),
    );
    let anchor_error = validate_canonical_protocol_storage(&oversized_anchor)
        .expect_err("full audit must enforce the user-anchor budget");
    assert!(
        anchor_error
            .to_string()
            .contains("exceeding the 20000-token checkpoint bound")
    );
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
    run(&connection).expect("complete V34 through V40 after V33 retry");
    run(&connection).expect("idempotent V40 validation after V33 retry");
    assert!(
        schema_migration_applied(&connection, FLATTEN_SESSION_SPAWN_EDGES_VERSION)
            .expect("V40 marker after retry")
    );
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
    let canonical_domain = "'pending', 'running', 'completed', 'declined', 'cancelled', 'failed'";
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

    run_tool_call_declined_cancelled_status_migration(&connection).expect("v31 forward migration");

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

    insert_tool_call(&connection, "declined", "declined", 40).expect("declined after migration");
    insert_tool_call(&connection, "cancelled", "cancelled", 41).expect("cancelled after migration");
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
fn v47_fresh_schema_accepts_recursive_lineage_and_rejects_invalid_parentage() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");

    run(&connection).expect("fresh migration");
    assert!(
        schema_migration_has_exact_name(
            &connection,
            RECURSIVE_SESSION_SPAWN_EDGES_VERSION,
            RECURSIVE_SESSION_SPAWN_EDGES_NAME,
        )
        .expect("V47 marker")
    );

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
            "spawn_order",
            "created_at_ms",
        ]
    );
    let mut statement = connection
        .prepare("PRAGMA table_info(agent_completion_handoffs)")
        .expect("completion handoff columns");
    let handoff_columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("handoff column rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("handoff columns");
    assert_eq!(
        handoff_columns,
        vec![
            "child_session_id",
            "child_turn_id",
            "child_terminal_event_id",
            "parent_session_id",
            "parent_history_item_id",
            "created_at_ms",
        ]
    );

    connection
            .execute_batch(
                "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('recursive-project', 'C:/recursive', 'recursive', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('recursive-root', 'recursive-project', 'root', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 1, 1, NULL),
                 ('recursive-parent', 'recursive-project', 'parent', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 2, 2, NULL),
                 ('recursive-grandchild', 'recursive-project', 'grandchild', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 3, 3, NULL),
                 ('wrong-path-child', 'recursive-project', 'wrong path', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 4, 4, NULL),
                 ('unattached-parent', 'recursive-project', 'unattached parent', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 5, 5, NULL),
                 ('unattached-child', 'recursive-project', 'unattached child', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 6, 6, NULL),
                 ('other-root', 'recursive-project', 'other root', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 7, 7, NULL),
                 ('other-parent', 'recursive-project', 'other parent', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 8, 8, NULL),
                 ('cross-tree-child', 'recursive-project', 'cross tree child', 'idle', 'C:/recursive',
                  'model', 'http://localhost', 9, 9, NULL);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES
                 ('recursive-root', 'recursive-root', 'recursive-parent',
                  '/root/parent', 'parent', 1, 2),
                 ('recursive-root', 'recursive-parent', 'recursive-grandchild',
                  '/root/parent/grandchild', 'grandchild', 2, 3),
                 ('other-root', 'other-root', 'other-parent',
                  '/root/other_parent', 'other_parent', 1, 8);",
            )
            .expect("valid recursive trees");

    for (sql, label) in [
        (
            "INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('recursive-root', 'recursive-parent', 'wrong-path-child',
                         '/root/wrong_path', 'wrong_path', 3, 10)",
            "wrong canonical parent path",
        ),
        (
            "INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('recursive-root', 'unattached-parent', 'unattached-child',
                         '/root/unattached_parent/unattached_child', 'unattached_child', 3, 11)",
            "unattached parent",
        ),
        (
            "INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('recursive-root', 'other-parent', 'cross-tree-child',
                         '/root/other_parent/cross_tree_child', 'cross_tree_child', 3, 12)",
            "cross-tree parent",
        ),
    ] {
        assert!(
            connection.execute(sql, []).is_err(),
            "V47 must reject {label}"
        );
    }

    run(&connection).expect("idempotent migration with recursive lineage");
    validate_recursive_session_spawn_edge_data(&connection)
        .expect("recursive lineage data contract");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM session_spawn_edges
                     WHERE root_session_id = 'recursive-root'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("recursive edge count"),
        2
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v47_marker_rejects_a_stale_recursive_lookup_index() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh migration");
    connection
        .execute_batch("DROP INDEX idx_session_spawn_edges_parent_created")
        .expect("corrupt V47 index");

    let error = run(&connection).expect_err("V47 marker must validate its lookup indexes");

    assert!(
        error
            .to_string()
            .contains("idx_session_spawn_edges_parent_created")
    );
}

#[test]
fn v47_marker_rejects_partial_unique_indexes_that_never_enforce_ownership() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh migration");
    let canonical_table_sql = connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'table' AND name = 'session_spawn_edges'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("canonical table SQL");
    let stale_table_sql = canonical_table_sql
        .replace("UNIQUE(root_session_id, agent_path),", "")
        .replace("UNIQUE(root_session_id, spawn_order),", "");
    assert_ne!(
        stale_table_sql, canonical_table_sql,
        "the fixture must remove both inline unique owners"
    );
    let owned_object_sql = {
        let mut statement = connection
            .prepare(
                "SELECT sql
                     FROM sqlite_master
                     WHERE tbl_name = 'session_spawn_edges'
                       AND type IN ('index', 'trigger')
                       AND sql IS NOT NULL
                     ORDER BY type ASC, name ASC",
            )
            .expect("owned schema objects");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("owned schema rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("owned schema SQL")
    };

    connection
        .execute_batch("DROP TABLE session_spawn_edges")
        .expect("drop canonical table");
    connection
        .execute_batch(&stale_table_sql)
        .expect("recreate stale table");
    for sql in owned_object_sql {
        connection
            .execute_batch(&sql)
            .expect("restore canonical explicit schema object");
    }
    connection
        .execute_batch(
            "CREATE UNIQUE INDEX stale_session_spawn_edges_root_path
                     ON session_spawn_edges(root_session_id, agent_path)
                     WHERE 0;
                 CREATE UNIQUE INDEX stale_session_spawn_edges_root_order
                     ON session_spawn_edges(root_session_id, spawn_order)
                     WHERE 0;",
        )
        .expect("non-enforcing unique indexes");

    let error =
        run(&connection).expect_err("V47 marker must reject partial indexes as unique ownership");

    assert!(error.to_string().contains("session_spawn_edges"));
    assert!(error.to_string().contains("stale SQL"));
}

#[test]
fn v47_marker_rejects_a_trigger_disabled_by_an_always_false_when_clause() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh migration");
    connection
        .execute_batch(
            "DROP TRIGGER limit_session_spawn_edges_per_root;
                 CREATE TRIGGER limit_session_spawn_edges_per_root
                 BEFORE INSERT ON session_spawn_edges
                 WHEN 0 AND (
                     SELECT COUNT(*)
                     FROM session_spawn_edges
                     WHERE root_session_id = NEW.root_session_id
                 ) >= 255
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'agent tree reached its retained-descendant capacity of 255'
                     );
                 END;",
        )
        .expect("disable capacity trigger while retaining its marker fragments");

    let error = run(&connection).expect_err("V47 marker must reject an always-disabled trigger");

    assert!(
        error
            .to_string()
            .contains("limit_session_spawn_edges_per_root")
    );
    assert!(error.to_string().contains("stale SQL"));
}

#[test]
fn v47_marker_rejects_a_disabled_completion_handoff_integrity_trigger() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh migration");
    connection
        .execute_batch(
            "DROP TRIGGER validate_agent_completion_handoff_before_insert;
                 CREATE TRIGGER validate_agent_completion_handoff_before_insert
                 BEFORE INSERT ON agent_completion_handoffs
                 WHEN 0
                 BEGIN
                     SELECT RAISE(ABORT, 'disabled handoff validation');
                 END;",
        )
        .expect("disable completion handoff trigger");

    let error =
        run(&connection).expect_err("V47 marker must reject a disabled handoff integrity trigger");
    assert!(
        error
            .to_string()
            .contains("validate_agent_completion_handoff_before_insert")
    );
    assert!(error.to_string().contains("stale SQL"));
}

#[test]
fn v47_upgrades_v46_direct_edges_and_is_idempotent() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v46(&connection);
    assert!(
        schema_migration_applied(&connection, CODEX_COMPACTION_CHECKPOINT_VERSION)
            .expect("V46 marker")
    );
    assert!(
        !schema_migration_applied(&connection, RECURSIVE_SESSION_SPAWN_EDGES_VERSION)
            .expect("no V47 marker")
    );
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('upgrade-project', 'C:/upgrade', 'upgrade', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('upgrade-root', 'upgrade-project', 'root', 'idle', 'C:/upgrade',
                  'model', 'http://localhost', 1, 1, NULL),
                 ('upgrade-parent', 'upgrade-project', 'parent', 'idle', 'C:/upgrade',
                  'model', 'http://localhost', 2, 2, NULL),
                 ('upgrade-grandchild', 'upgrade-project', 'grandchild', 'idle', 'C:/upgrade',
                  'model', 'http://localhost', 3, 3, NULL);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, created_at_ms)
                 VALUES ('upgrade-root', 'upgrade-root', 'upgrade-parent',
                         '/root/parent', 'parent', 2);",
        )
        .expect("V46 direct-edge fixture");
    let direct_edge_before = text_snapshot(
        &connection,
        "SELECT json_group_array(json_array(root_session_id, parent_session_id,
                                                child_session_id, agent_path, task_name,
                                                created_at_ms))
             FROM session_spawn_edges",
    );

    run(&connection).expect("V46 to V47 upgrade");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            RECURSIVE_SESSION_SPAWN_EDGES_VERSION,
            RECURSIVE_SESSION_SPAWN_EDGES_NAME,
        )
        .expect("V47 marker")
    );
    assert_eq!(
        text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(root_session_id, parent_session_id,
                                                    child_session_id, agent_path, task_name,
                                                    created_at_ms))
                 FROM session_spawn_edges",
        ),
        direct_edge_before
    );
    connection
        .execute(
            "INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('upgrade-root', 'upgrade-parent', 'upgrade-grandchild',
                         '/root/parent/grandchild', 'grandchild', 2, 3)",
            [],
        )
        .expect("nested descendant after V47");

    run(&connection).expect("idempotent V47 validation");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM session_spawn_edges
                     WHERE root_session_id = 'upgrade-root'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("upgraded edge count"),
        2
    );
    validate_recursive_session_spawn_edge_data(&connection)
        .expect("upgraded recursive data contract");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v49_upgrades_v47_schema_and_is_idempotent() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v46(&connection);
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for V47 endpoint");
    connection
        .execute_batch(V47_RECURSIVE_SESSION_SPAWN_EDGES)
        .expect("create exact V47 endpoint");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
    assert!(
        schema_migration_has_exact_name(
            &connection,
            RECURSIVE_SESSION_SPAWN_EDGES_VERSION,
            RECURSIVE_SESSION_SPAWN_EDGES_NAME,
        )
        .expect("V47 marker")
    );
    assert!(
        !schema_migration_applied(&connection, AGENT_OWNER_RESUME_REQUESTS_VERSION)
            .expect("V48 absent")
    );
    assert!(
        !schema_migration_applied(&connection, AGENT_TREE_STOP_FENCES_VERSION).expect("V49 absent")
    );

    run(&connection).expect("V47 to V49 upgrade");
    run(&connection).expect("idempotent V49 validation");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            AGENT_OWNER_RESUME_REQUESTS_VERSION,
            AGENT_OWNER_RESUME_REQUESTS_NAME,
        )
        .expect("V48 marker")
    );
    assert!(
        schema_migration_has_exact_name(
            &connection,
            AGENT_TREE_STOP_FENCES_VERSION,
            AGENT_TREE_STOP_FENCES_NAME,
        )
        .expect("V49 marker")
    );
    validate_agent_owner_resume_request_schema(&connection).expect("V49 exact schema");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v49_upgrades_exact_v48_endpoint_and_is_idempotent() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v46(&connection);
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for V48 endpoint");
    connection
        .execute_batch(V47_RECURSIVE_SESSION_SPAWN_EDGES)
        .expect("create exact V47 endpoint");
    connection
        .execute_batch(V48_AGENT_OWNER_RESUME_REQUESTS)
        .expect("create exact V48 endpoint");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
    assert!(
        schema_migration_has_exact_name(
            &connection,
            AGENT_OWNER_RESUME_REQUESTS_VERSION,
            AGENT_OWNER_RESUME_REQUESTS_NAME,
        )
        .expect("V48 marker")
    );
    assert!(
        !schema_migration_applied(&connection, AGENT_TREE_STOP_FENCES_VERSION).expect("V49 absent")
    );

    run(&connection).expect("V48 to V49 upgrade");
    run(&connection).expect("idempotent V49 validation");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            AGENT_TREE_STOP_FENCES_VERSION,
            AGENT_TREE_STOP_FENCES_NAME,
        )
        .expect("V49 marker")
    );
    validate_agent_owner_resume_request_schema(&connection).expect("V49 exact schema");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v49_backfills_tree_close_fences_and_cancels_v48_late_result_resume_seed() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v46(&connection);
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for exact V47 endpoint");
    connection
        .execute_batch(V47_RECURSIVE_SESSION_SPAWN_EDGES)
        .expect("create exact V47 endpoint");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('backfill-project', 'C:/backfill', 'backfill',
                         'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('backfill-root', 'backfill-project', 'root', 'cancelled',
                  'C:/backfill', 'model', 'http://localhost', 1, 4, 4),
                 ('backfill-owner', 'backfill-project', 'owner', 'completed',
                  'C:/backfill', 'model', 'http://localhost', 2, 5, 5),
                 ('backfill-child', 'backfill-project', 'child', 'completed',
                  'C:/backfill', 'model', 'http://localhost', 3, 6, 6),
                 ('backfill-failed-root', 'backfill-project', 'failed root',
                  'failed', 'C:/backfill', 'model', 'http://localhost',
                  7, 8, 8);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES
                 ('backfill-root', 'backfill-root', 'backfill-owner',
                  '/root/owner', 'owner', 1, 2),
                 ('backfill-root', 'backfill-owner', 'backfill-child',
                  '/root/owner/child', 'child', 2, 3);",
        )
        .expect("exact V47 tree fixture");

    let warning_payload = r#"{"kind":"warning","message":"thread started: migration fixture"}"#;
    let user_stop_payload = r#"{"kind":"turn_terminal","terminal":{"outcome":{"kind":"interrupted","cause":"user_stop"}}}"#;
    let completed_payload =
        r#"{"kind":"turn_terminal","terminal":{"outcome":{"kind":"completed"}}}"#;
    let failed_payload = r#"{"kind":"turn_terminal","terminal":{"outcome":{"kind":"failed"}}}"#;
    let tree_stopped_payload = r#"{"kind":"turn_terminal","terminal":{"outcome":{"kind":"interrupted","cause":"tree_stopped"}}}"#;
    for (event_id, session_id, turn_id, sequence_no, payload, created_at_ms) in [
        (
            "backfill-child-start",
            "backfill-child",
            "backfill-child-turn",
            0_i64,
            warning_payload,
            3_i64,
        ),
        (
            "backfill-owner-start",
            "backfill-owner",
            "backfill-owner-turn",
            0,
            warning_payload,
            3,
        ),
        (
            "backfill-owner-terminal",
            "backfill-owner",
            "backfill-owner-turn",
            1,
            completed_payload,
            3,
        ),
        (
            "backfill-root-stop",
            "backfill-root",
            "backfill-root-turn",
            0,
            user_stop_payload,
            4,
        ),
        (
            "backfill-child-terminal",
            "backfill-child",
            "backfill-child-turn",
            1,
            completed_payload,
            6,
        ),
        (
            "backfill-failed-start",
            "backfill-failed-root",
            "backfill-failed-turn",
            0,
            warning_payload,
            7,
        ),
        (
            "backfill-failed-terminal",
            "backfill-failed-root",
            "backfill-failed-turn",
            1,
            failed_payload,
            8,
        ),
        (
            "backfill-followup-start",
            "backfill-child",
            "backfill-followup-turn",
            0,
            warning_payload,
            9,
        ),
        (
            "backfill-followup-stop",
            "backfill-child",
            "backfill-followup-turn",
            1,
            tree_stopped_payload,
            10,
        ),
    ] {
        connection
            .execute(
                "INSERT INTO protocol_runtime_events
                     (id, session_id, turn_id, sequence_no, msg_json,
                      payload_sha256, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event_id,
                    session_id,
                    turn_id,
                    sequence_no,
                    payload,
                    sha256_text(payload),
                    created_at_ms
                ],
            )
            .expect("backfill runtime event");
        connection
            .execute(
                "INSERT INTO protocol_item_append_order
                     (session_id, scope_kind, turn_id, sequence_no,
                      source_kind, source_id, created_at_ms)
                     VALUES (?1, 'turn', ?2, ?3, 'runtime_event', ?4, ?5)",
                params![session_id, turn_id, sequence_no, event_id, created_at_ms],
            )
            .expect("backfill runtime append order");
    }

    let final_payload = serde_json::json!({
            "kind": "inter_agent_communication",
            "communication": {
                "author": "/root/owner/child",
                "recipient": "/root/owner",
                "content": "Message Type: FINAL_ANSWER\nTask name: /root/owner\nSender: /root/owner/child\nPayload:\nlate result",
                "trigger_turn": false
            }
        })
        .to_string();
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no,
                  payload_json, payload_sha256, created_at_ms)
                 VALUES ('backfill-final', 'backfill-owner', 'session', NULL, 0,
                         ?1, ?2, 6)",
            params![final_payload, sha256_text(&final_payload)],
        )
        .expect("late child FINAL");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES ('backfill-owner', 'session', NULL, 0,
                         'history_item', 'backfill-final', 6)",
            [],
        )
        .expect("late child FINAL append order");
    connection
        .execute(
            "INSERT INTO agent_completion_handoffs
                 (child_session_id, child_turn_id, child_terminal_event_id,
                  parent_session_id, parent_history_item_id, created_at_ms)
                 VALUES ('backfill-child', 'backfill-child-turn',
                         'backfill-child-terminal', 'backfill-owner',
                         'backfill-final', 6)",
            [],
        )
        .expect("late child handoff");

    connection
        .execute_batch(V48_AGENT_OWNER_RESUME_REQUESTS)
        .expect("exact V48 endpoint with seeded late result");
    connection
        .execute(
            "INSERT OR IGNORE INTO agent_deferred_completions
                 (agent_session_id, agent_turn_id, terminal_event_id,
                  parent_session_id, kind, state,
                  resolved_by_terminal_event_id, created_at_ms, updated_at_ms,
                  resolved_at_ms)
                 VALUES ('backfill-owner', 'backfill-owner-turn',
                         'backfill-owner-terminal', 'backfill-root',
                         'completed_early', 'pending', NULL, 3, 3, NULL)",
            [],
        )
        .expect("legacy V48 pending deferred receipt");

    run(&connection).expect("V48 through V49 backfill");

    assert_eq!(
        connection
            .query_row(
                "SELECT cause
                     FROM agent_tree_stop_fences
                     WHERE stopped_session_id = 'backfill-root'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("backfilled root Stop fence"),
        "user_stop"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT cause
                     FROM agent_tree_stop_fences
                     WHERE stopped_session_id = 'backfill-failed-root'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("backfilled root Failed fence"),
        "root_failed"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM agent_tree_stop_fences
                     WHERE stopped_session_id = 'backfill-child'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("post-fence follow-up child fence count"),
        0,
        "a later TreeStopped terminal must not invent a new child generation"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT state
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = 'backfill-owner'
                       AND source_history_item_id = 'backfill-final'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("V48 seed resolved by V49"),
        "cancelled"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT state || ':' || resolved_by_terminal_event_id
                     FROM agent_deferred_completions
                     WHERE agent_session_id = 'backfill-owner'
                       AND agent_turn_id = 'backfill-owner-turn'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("V49 physical deferred sweep"),
        "discarded:backfill-owner-terminal"
    );
    validate_agent_owner_resume_request_data(&connection).expect("backfilled V49 durable data");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v49_tree_stop_fence_rejects_invalid_subtree_and_full_audit_detects_tampering() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh current schema");
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('fence-project', 'C:/fence', 'fence', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms)
                 VALUES
                 ('fence-root', 'fence-project', 'root', 'idle', 'C:/fence',
                  'model', 'http://localhost', 1, 1),
                 ('fence-orphan', 'fence-project', 'orphan', 'idle', 'C:/fence',
                  'model', 'http://localhost', 2, 2);",
        )
        .expect("fence identity fixtures");

    let direct_error = connection
        .execute(
            "INSERT INTO agent_tree_stop_fences
                 (root_session_id, stopped_session_id, after_append_position,
                  cause, created_at_ms)
                 VALUES ('fence-root', 'fence-orphan', 0, 'user_stop', 3)",
            [],
        )
        .expect_err("unowned stopped session must be rejected");
    assert!(
        direct_error
            .to_string()
            .contains("stopped session must belong to the canonical tree"),
        "unexpected insert error: {direct_error}"
    );

    let validation_trigger_sql = connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name =
                       'validate_agent_tree_stop_fence_subtree_before_insert'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("canonical fence validation trigger");
    connection
        .execute_batch("DROP TRIGGER validate_agent_tree_stop_fence_subtree_before_insert")
        .expect("temporarily bypass fence validation");
    connection
        .execute(
            "INSERT INTO agent_tree_stop_fences
                 (root_session_id, stopped_session_id, after_append_position,
                  cause, created_at_ms)
                 VALUES ('fence-root', 'fence-orphan', 0, 'user_stop', 3)",
            [],
        )
        .expect("tampered fence row");
    connection
        .execute_batch(&validation_trigger_sql)
        .expect("restore canonical validation trigger");
    validate_agent_owner_resume_request_schema(&connection)
        .expect("tamper fixture retains exact V49 schema");

    let error = validate_canonical_protocol_storage(&connection)
        .expect_err("full storage audit must reject tampered fence identity");
    assert!(
        error.to_string().contains("tree-stop fence row"),
        "unexpected full-audit error: {error}"
    );
}

#[test]
fn v49_marker_rejects_a_missing_effective_deferred_completion_view() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    connection
        .execute_batch("DROP VIEW effective_agent_deferred_completions")
        .expect("remove effective deferred-completion view");

    let error = run(&connection).expect_err("V49 marker must own the exact effective view");
    assert!(
        error
            .to_string()
            .contains("effective_agent_deferred_completions"),
        "unexpected schema error: {error}"
    );
}

#[test]
fn v49_tree_stop_fences_cascade_with_stopped_session_deletion() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh current schema");
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('delete-fence-project', 'C:/delete-fence',
                         'delete fence', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms)
                 VALUES
                 ('delete-fence-root', 'delete-fence-project', 'root', 'idle',
                  'C:/delete-fence', 'model', 'http://localhost', 1, 1),
                 ('delete-fence-child', 'delete-fence-project', 'child', 'idle',
                  'C:/delete-fence', 'model', 'http://localhost', 2, 2),
                 ('delete-fence-solo', 'delete-fence-project', 'solo', 'idle',
                  'C:/delete-fence', 'model', 'http://localhost', 3, 3);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('delete-fence-root', 'delete-fence-root',
                         'delete-fence-child', '/root/child', 'child', 1, 2);
                 INSERT INTO agent_tree_stop_fences
                 (root_session_id, stopped_session_id, after_append_position,
                  cause, created_at_ms)
                 VALUES
                 ('delete-fence-root', 'delete-fence-child', 0, 'user_stop', 4),
                 ('delete-fence-solo', 'delete-fence-solo', 0, 'root_failed', 5);",
        )
        .expect("deletable fence fixtures");

    connection
        .execute(
            "DELETE FROM sessions
                 WHERE id IN ('delete-fence-child', 'delete-fence-solo')",
            [],
        )
        .expect("delete stopped subtree and standalone stopped root");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| row
                .get::<_, i64>(0),)
            .expect("remaining fence count"),
        0
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v49_fence_boundary_survives_append_rollback_watermark_and_reopen() {
    let temp = tempfile::tempdir().expect("tempdir");
    let database_path = temp.path().join("fence-watermark.sqlite");
    {
        let connection = Connection::open(&database_path).expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run(&connection).expect("fresh current schema");
        connection
            .execute_batch(
                "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind,
                      created_at_ms, updated_at_ms)
                     VALUES ('watermark-project', 'C:/watermark',
                             'watermark', 'none', 1, 1);
                     INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name,
                      base_url, created_at_ms, updated_at_ms)
                     VALUES ('watermark-root', 'watermark-project', 'root',
                             'idle', 'C:/watermark', 'model',
                             'http://localhost', 1, 1);
                     INSERT INTO protocol_item_append_order
                     (session_id, scope_kind, turn_id, sequence_no,
                      source_kind, source_id, created_at_ms)
                     VALUES ('watermark-root', 'turn', 'rolled-back-turn', 0,
                             'runtime_event', 'rolled-back-source', 2);
                     DELETE FROM protocol_item_append_order
                     WHERE source_id = 'rolled-back-source';
                     INSERT INTO agent_tree_stop_fences
                     (root_session_id, stopped_session_id,
                      after_append_position, cause, created_at_ms)
                     VALUES ('watermark-root', 'watermark-root', 1,
                             'user_stop', 3);",
            )
            .expect("fence at retained AUTOINCREMENT watermark");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM protocol_item_append_order",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("live append count"),
            0
        );
        validate_canonical_protocol_storage(&connection).expect("watermark-backed fence audit");
    }

    let reopened = Connection::open(&database_path).expect("reopened database");
    reopened
        .pragma_update(None, "foreign_keys", "ON")
        .expect("reopened foreign keys");
    run(&reopened).expect("reopened current migration");
    validate_canonical_protocol_storage(&reopened).expect("reopened watermark-backed fence audit");
    assert_eq!(
        reopened
            .query_row(
                "SELECT after_append_position
                     FROM agent_tree_stop_fences
                     WHERE stopped_session_id = 'watermark-root'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("reopened fence"),
        1
    );
}

#[test]
fn v49_fence_physically_discards_pre_fence_receipt_and_allows_post_fence_pending() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh current schema");
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('discard-project', 'C:/discard', 'discard',
                         'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('discard-root', 'discard-project', 'root', 'idle',
                  'C:/discard', 'model', 'http://localhost', 1, 1, NULL),
                 ('discard-owner', 'discard-project', 'owner', 'completed',
                  'C:/discard', 'model', 'http://localhost', 2, 2, 2),
                 ('discard-child', 'discard-project', 'child', 'completed',
                  'C:/discard', 'model', 'http://localhost', 3, 3, 3);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES
                 ('discard-root', 'discard-root', 'discard-owner',
                  '/root/owner', 'owner', 1, 2),
                 ('discard-root', 'discard-owner', 'discard-child',
                  '/root/owner/child', 'child', 2, 3);",
        )
        .expect("discard tree fixture");

    let warning_payload = r#"{"kind":"warning","message":"thread started: discard fixture"}"#;
    let completed_payload =
        r#"{"kind":"turn_terminal","terminal":{"outcome":{"kind":"completed"}}}"#;
    for (event_id, session_id, turn_id, sequence_no, payload, created_at_ms) in [
        (
            "discard-owner-start",
            "discard-owner",
            "discard-owner-turn",
            0_i64,
            warning_payload,
            2_i64,
        ),
        (
            "discard-owner-terminal",
            "discard-owner",
            "discard-owner-turn",
            1,
            completed_payload,
            3,
        ),
    ] {
        connection
            .execute(
                "INSERT INTO protocol_runtime_events
                     (id, session_id, turn_id, sequence_no, msg_json,
                      payload_sha256, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event_id,
                    session_id,
                    turn_id,
                    sequence_no,
                    payload,
                    sha256_text(payload),
                    created_at_ms
                ],
            )
            .expect("owner runtime event");
        connection
            .execute(
                "INSERT INTO protocol_item_append_order
                     (session_id, scope_kind, turn_id, sequence_no,
                      source_kind, source_id, created_at_ms)
                     VALUES (?1, 'turn', ?2, ?3, 'runtime_event', ?4, ?5)",
                params![session_id, turn_id, sequence_no, event_id, created_at_ms],
            )
            .expect("owner append order");
    }
    connection
        .execute(
            "INSERT INTO agent_deferred_completions
                 (agent_session_id, agent_turn_id, terminal_event_id,
                  parent_session_id, kind, state,
                  resolved_by_terminal_event_id, created_at_ms, updated_at_ms,
                  resolved_at_ms)
                 VALUES ('discard-owner', 'discard-owner-turn',
                         'discard-owner-terminal', 'discard-root',
                         'completed_early', 'pending', NULL, 3, 3, NULL)",
            [],
        )
        .expect("pre-fence deferred owner");
    connection
        .execute(
            "INSERT INTO agent_tree_stop_fences
                 (root_session_id, stopped_session_id, after_append_position,
                  cause, created_at_ms)
                 VALUES ('discard-root', 'discard-child', 2, 'user_stop', 4)",
            [],
        )
        .expect("descendant stop fence");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM effective_agent_deferred_completions
                     WHERE agent_session_id = 'discard-owner'
                       AND state = 'pending'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("effective pending count after descendant fence"),
        0
    );
    connection
        .execute(
            "UPDATE agent_deferred_completions
                 SET state = 'discarded',
                     resolved_by_terminal_event_id = terminal_event_id,
                     updated_at_ms = 4,
                     resolved_at_ms = 4
                 WHERE agent_session_id = 'discard-owner'
                   AND agent_turn_id = 'discard-owner-turn'",
            [],
        )
        .expect("physical fence-backed deferred discard");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM effective_agent_deferred_completions
                     WHERE agent_session_id = 'discard-owner'
                       AND state = 'discarded'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("effective forensic receipt count"),
        1
    );

    for (event_id, sequence_no, payload, created_at_ms) in [
        ("discard-followup-start", 0_i64, warning_payload, 5_i64),
        ("discard-followup-terminal", 1_i64, completed_payload, 6_i64),
    ] {
        connection
            .execute(
                "INSERT INTO protocol_runtime_events
                     (id, session_id, turn_id, sequence_no, msg_json,
                      payload_sha256, created_at_ms)
                     VALUES (?1, 'discard-owner', 'discard-followup-turn',
                             ?2, ?3, ?4, ?5)",
                params![
                    event_id,
                    sequence_no,
                    payload,
                    sha256_text(payload),
                    created_at_ms
                ],
            )
            .expect("post-fence follow-up event");
        connection
            .execute(
                "INSERT INTO protocol_item_append_order
                     (session_id, scope_kind, turn_id, sequence_no,
                      source_kind, source_id, created_at_ms)
                     VALUES ('discard-owner', 'turn', 'discard-followup-turn',
                             ?1, 'runtime_event', ?2, ?3)",
                params![sequence_no, event_id, created_at_ms],
            )
            .expect("post-fence follow-up append order");
    }

    connection
        .execute(
            "INSERT INTO agent_deferred_completions
                 (agent_session_id, agent_turn_id, terminal_event_id,
                  parent_session_id, kind, state,
                  resolved_by_terminal_event_id, created_at_ms, updated_at_ms,
                  resolved_at_ms)
                 VALUES ('discard-owner', 'discard-followup-turn',
                         'discard-followup-terminal', 'discard-root',
                         'completed_early', 'pending', NULL, 6, 6, NULL)",
            [],
        )
        .expect("post-fence pending receipt reuses the unique owner slot");
    connection
        .execute(
            "UPDATE agent_deferred_completions
                 SET state = 'discarded',
                     resolved_by_terminal_event_id = 'discard-owner-terminal',
                     updated_at_ms = 7,
                     resolved_at_ms = 7
                 WHERE agent_session_id = 'discard-owner'
                   AND agent_turn_id = 'discard-followup-turn'",
            [],
        )
        .expect_err("an old terminal must not discard the post-fence receipt");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM effective_agent_deferred_completions
                     WHERE agent_session_id = 'discard-owner'
                       AND state = 'pending'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("effective post-fence pending count"),
        1
    );
    validate_agent_owner_resume_request_data(&connection)
        .expect("fence-backed discarded receipt is canonical");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v47_backfills_existing_terminal_final_pairs_without_rejecting_a_missing_legacy_final() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_v46(&connection);
    let root_session_id = crate::session::SessionId::new();
    let paired_child_id = crate::session::SessionId::new();
    let missing_child_id = crate::session::SessionId::new();
    connection
        .execute(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('handoff-upgrade-project', 'C:/upgrade-handoff',
                         'upgrade handoff', 'none', 1, 1)",
            [],
        )
        .expect("project");
    for (index, (session_id, title, status)) in [
        (root_session_id, "root", "idle"),
        (paired_child_id, "paired child", "completed"),
        (missing_child_id, "missing child", "completed"),
    ]
    .into_iter()
    .enumerate()
    {
        connection
            .execute(
                "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES (?1, 'handoff-upgrade-project', ?2, ?3,
                             'C:/upgrade-handoff', 'model', 'http://localhost',
                             ?4, ?4, CASE WHEN ?3 = 'completed' THEN ?4 ELSE NULL END)",
                params![
                    session_id.to_string(),
                    title,
                    status,
                    i64::try_from(index + 1).expect("fixture time")
                ],
            )
            .expect("session");
    }
    connection
        .execute(
            "INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, created_at_ms)
                 VALUES
                 (?1, ?1, ?2, '/root/paired', 'paired', 2),
                 (?1, ?1, ?3, '/root/missing', 'missing', 3)",
            params![
                root_session_id.to_string(),
                paired_child_id.to_string(),
                missing_child_id.to_string()
            ],
        )
        .expect("V46 edges");

    for (index, child_session_id) in [paired_child_id, missing_child_id].into_iter().enumerate() {
        let turn_id = crate::protocol::TurnId::new();
        let event_id = crate::protocol::RuntimeEventId::new();
        let terminal = crate::protocol::RuntimeEventMsg::TurnTerminal {
            terminal: Box::new(crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        let terminal_json = serde_json::to_string(&terminal).expect("terminal JSON");
        let created_at_ms = 10 + i64::try_from(index).expect("fixture time");
        connection
            .execute(
                "INSERT INTO protocol_runtime_events
                     (id, session_id, turn_id, sequence_no, msg_json,
                      payload_sha256, created_at_ms)
                     VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
                params![
                    event_id.to_string(),
                    child_session_id.to_string(),
                    turn_id.to_string(),
                    terminal_json,
                    sha256_text(&terminal_json),
                    created_at_ms
                ],
            )
            .expect("legacy terminal");
        connection
            .execute(
                "INSERT INTO protocol_item_append_order
                     (session_id, scope_kind, turn_id, sequence_no,
                      source_kind, source_id, created_at_ms)
                     VALUES (?1, 'turn', ?2, 0, 'runtime_event', ?3, ?4)",
                params![
                    child_session_id.to_string(),
                    turn_id.to_string(),
                    event_id.to_string(),
                    created_at_ms
                ],
            )
            .expect("terminal append order");
    }

    let parent_history_id = crate::protocol::HistoryItemId::new();
    let communication = crate::protocol::InterAgentCommunication {
        author: "/root/paired".to_string(),
        recipient: "/root".to_string(),
        content: crate::protocol::render_inter_agent_message(
            crate::protocol::InterAgentMessageType::FinalAnswer,
            "/root",
            "/root/paired",
            "legacy delivered result",
        ),
        trigger_turn: false,
    };
    let history_json = serde_json::to_string(
        &crate::protocol::HistoryItemPayload::InterAgentCommunication { communication },
    )
    .expect("parent FINAL JSON");
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no,
                  payload_json, payload_sha256, created_at_ms)
                 VALUES (?1, ?2, 'session', NULL, 0, ?3, ?4, 20)",
            params![
                parent_history_id.to_string(),
                root_session_id.to_string(),
                history_json,
                sha256_text(&history_json)
            ],
        )
        .expect("legacy parent FINAL");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES (?1, 'session', NULL, 0, 'history_item', ?2, 20)",
            params![root_session_id.to_string(), parent_history_id.to_string()],
        )
        .expect("parent FINAL append order");

    run(&connection).expect("V46 to V47 handoff upgrade");
    let receipts = connection
        .prepare(
            "SELECT child_session_id, parent_history_item_id
                 FROM agent_completion_handoffs
                 ORDER BY child_session_id",
        )
        .expect("receipt query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("receipt rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("receipts");
    assert_eq!(
        receipts,
        vec![(paired_child_id.to_string(), parent_history_id.to_string())]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM protocol_runtime_events
                     WHERE session_id = ?1
                       AND json_extract(msg_json, '$.kind') = 'turn_terminal'",
                params![missing_child_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("missing-final terminal remains"),
        1
    );
    run(&connection).expect("idempotent handoff backfill");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM agent_completion_handoffs",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .expect("idempotent receipt count"),
        1
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
                 (root_session_id, parent_session_id, child_session_id, agent_path, task_name,
                  spawn_order, created_at_ms)
                 VALUES ('root', 'root', 'child', '/root/child', 'child', 1, 3)",
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
fn v44_rejects_preexisting_duplicate_turn_terminals_and_rolls_back() {
    let connection = Connection::open_in_memory().expect("database");
    run_through_v43(&connection);
    let terminal = crate::session::DurableTurnTerminal {
        outcome: crate::protocol::TurnTerminalOutcome::Completed,
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    let msg_json = serde_json::json!({
        "kind": "turn_terminal",
        "terminal": terminal,
    })
    .to_string();
    connection
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES
                 ('terminal-1', 'session', 'turn', 0, ?1, 'hash-1', 1),
                 ('terminal-2', 'session', 'turn', 1, ?1, 'hash-2', 2)",
            [&msg_json],
        )
        .expect("duplicate terminal fixture");

    let error = run(&connection).expect_err("V44 must reject duplicate terminal owners");
    assert!(error.to_string().contains("UNIQUE constraint failed"));
    assert!(
        !schema_migration_applied(&connection, UNIQUE_TURN_TERMINAL_VERSION)
            .expect("rolled-back V44 marker")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_runtime_events
                     WHERE session_id = 'session' AND turn_id = 'turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("retained duplicate rows"),
        2
    );
}

#[test]
fn v44_marker_requires_the_exact_partial_unique_index() {
    let connection = Connection::open_in_memory().expect("database");
    run(&connection).expect("fresh current schema");
    connection
        .execute_batch("DROP INDEX idx_protocol_runtime_events_unique_turn_terminal")
        .expect("drop V44 index");

    let error = run(&connection).expect_err("stale V44 schema must fail closed");
    assert!(error.to_string().contains("V44 marker"));
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

#[test]
fn current_schema_fast_path_audits_durable_mailbox_identity() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh migration");
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('project', 'C:/workspace', 'workspace', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('session', 'project', 'session', 'completed', 'C:/workspace', 'model',
                  'http://localhost', 1, 1, 1),
                 ('owner', 'project', 'owner', 'idle', 'C:/workspace', 'model',
                  'http://localhost', 2, 2, NULL),
                 ('child', 'project', 'child', 'completed', 'C:/workspace', 'model',
                  'http://localhost', 3, 3, 3);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES
                 ('session', 'session', 'owner', '/root/owner', 'owner', 1, 2),
                 ('session', 'owner', 'child', '/root/owner/child', 'child', 2, 3);",
        )
        .expect("current-schema corruption fixture");
    let resume_payload = serde_json::json!({
            "kind": "inter_agent_communication",
            "communication": {
                "author": "/root/owner/child",
                "recipient": "/root/owner",
                "content": "Message Type: FINAL_ANSWER\nTask name: /root/owner\nSender: /root/owner/child\nPayload:\ncompleted",
                "trigger_turn": false
            }
        })
        .to_string();
    let child_terminal_payload =
        serde_json::to_string(&crate::protocol::RuntimeEventMsg::TurnTerminal {
            terminal: Box::new(crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        })
        .expect("child terminal payload");
    connection
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json,
                  payload_sha256, created_at_ms)
                 VALUES ('resume-terminal', 'child', 'child-turn', 0, ?1, ?2, 3)",
            params![child_terminal_payload, sha256_text(&child_terminal_payload)],
        )
        .expect("valid V48 child terminal");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES ('child', 'turn', 'child-turn', 0,
                         'runtime_event', 'resume-terminal', 3)",
            [],
        )
        .expect("valid V48 child terminal append order");
    connection
        .execute(
            "INSERT INTO agent_mailbox_messages
                 (id, root_session_id, author_session_id, recipient_session_id,
                  payload_json, payload_sha256, trigger_turn, state,
                  delivered_turn_id, delivered_history_item_id,
                  resolved_by_terminal_event_id,
                  discarded_by_stopped_session_id,
                  discarded_after_append_position, created_at_ms,
                  updated_at_ms, resolved_at_ms)
                 VALUES ('resume-source', 'session', 'child', 'owner',
                         ?1, ?2, 0, 'pending',
                         NULL, NULL, NULL, NULL, NULL, 4, 4, NULL)",
            params![resume_payload, sha256_text(&resume_payload)],
        )
        .expect("valid V50 source mailbox");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES ('owner', 'session', NULL, 2,
                         'mailbox_message', 'resume-source', 4)",
            [],
        )
        .expect("valid V50 source mailbox append order");
    connection
        .execute(
            "INSERT INTO agent_completion_handoffs
                 (child_session_id, child_turn_id, child_terminal_event_id,
                  parent_session_id, parent_history_item_id, created_at_ms)
                 VALUES ('child', 'child-turn', 'resume-terminal',
                         'owner', 'resume-source', 4)",
            [],
        )
        .expect("valid V48 direct-child handoff");
    connection
        .execute(
            "INSERT INTO agent_owner_resume_requests
                 (owner_session_id, source_session_id, source_history_item_id,
                  state, claimed_turn_id, created_at_ms, updated_at_ms,
                  claimed_at_ms, resolved_at_ms)
                 VALUES ('owner', 'owner', 'resume-source', 'pending',
                         NULL, 4, 4, NULL, NULL)",
            [],
        )
        .expect("valid V48 request");
    let corrupt_resume_payload = serde_json::json!({
        "kind": "inter_agent_communication",
        "communication": {
            "author": "/root/owner/child",
            "recipient": "/root/wrong_owner",
            "content": "corrupt owner resume source",
            "trigger_turn": false
        }
    })
    .to_string();
    let mailbox_validation_trigger = connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'validate_agent_mailbox_message_before_update'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("mailbox update trigger");
    connection
        .execute_batch("DROP TRIGGER validate_agent_mailbox_message_before_update")
        .expect("temporarily bypass mailbox update validation");
    connection
        .execute(
            "UPDATE agent_mailbox_messages
                 SET payload_json = ?2, payload_sha256 = ?3
                 WHERE id = ?1",
            params![
                "resume-source",
                corrupt_resume_payload,
                sha256_text(&corrupt_resume_payload)
            ],
        )
        .expect("corrupt V48 source after its insert trigger");
    connection
        .execute_batch(&mailbox_validation_trigger)
        .expect("restore mailbox update trigger");

    let error = run(&connection)
        .expect_err("current schema fast path must reject corrupt mailbox identity");
    assert!(
        error.to_string().contains("mailbox message(s) violate"),
        "unexpected current-schema audit error: {error}"
    );
}

fn run_through_exact_v49_endpoint(connection: &Connection) {
    run_through_v46(connection);
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for exact released endpoint");
    connection
        .execute_batch(V47_RECURSIVE_SESSION_SPAWN_EDGES)
        .expect("V47 endpoint");
    connection
        .execute_batch(V48_AGENT_OWNER_RESUME_REQUESTS)
        .expect("V48 endpoint");
    connection
        .execute_batch(V49_AGENT_TREE_STOP_FENCES)
        .expect("V49 endpoint");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
}

fn run_through_exact_v50_endpoint(connection: &Connection) {
    run_through_exact_v49_endpoint(connection);
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for exact V50 endpoint");
    connection
        .execute_batch(V50_DURABLE_AGENT_MAILBOX)
        .expect("V50 endpoint");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
}

fn run_through_exact_v51_endpoint(connection: &Connection) {
    run_through_exact_v50_endpoint(connection);
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for exact V51 endpoint");
    connection
        .execute_batch(V51_DURABLE_TURN_INPUT_QUEUE)
        .expect("V51 endpoint");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
}

fn run_through_exact_v55_endpoint(connection: &Connection) {
    run_through_exact_v51_endpoint(connection);
    connection
        .execute_batch(V52_HARNESS_TURN_IDENTITY)
        .expect("V52 endpoint");
    connection
        .execute_batch(V53_AGENT_TRIGGER_TURN_CLAIMS)
        .expect("V53 endpoint");
    connection
        .execute_batch(V54_PERMISSION_RETRY_FENCES)
        .expect("V54 endpoint");
    connection
        .execute_batch(V55_DURABLE_SIDE_CHATS)
        .expect("V55 endpoint");
}

fn run_through_exact_v56_endpoint(connection: &Connection) {
    run_through_exact_v55_endpoint(connection);
    connection
        .execute_batch(V56_ROLLBACK_HARNESS_ORPHAN_RECOVERY)
        .expect("V56 endpoint");
}

fn run_through_exact_v57_endpoint(connection: &Connection) {
    run_through_exact_v56_endpoint(connection);
    connection
        .execute_batch(V57_SESSION_ADMISSION_REVISIONS)
        .expect("V57 endpoint");
}

fn run_through_exact_v59_endpoint(connection: &Connection) {
    run_through_exact_v57_endpoint(connection);
    connection
        .execute_batch(V58_EXACT_EXECUTION_INTERRUPT_REQUESTS)
        .expect("V58 endpoint");
    connection
        .execute_batch(V59_SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW)
        .expect("V59 endpoint");
}

fn insert_v56_session(connection: &Connection) -> String {
    let session_id = crate::session::SessionId::new().to_string();
    insert_v56_session_with_id(connection, &session_id);
    session_id
}

fn insert_v56_session_with_id(connection: &Connection, session_id: &str) {
    let project_id = crate::session::ProjectId::new().to_string();
    connection
        .execute(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, 'v56', 'none', 1, 1)",
            params![project_id, format!("C:/v56/{project_id}")],
        )
        .expect("V56 project");
    connection
        .execute(
            "INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES (?1, ?2, 'session', 'idle', 'C:/v56', 'model',
                         'http://localhost', 1, 1, NULL)",
            params![session_id, project_id],
        )
        .expect("V56 session");
}

fn insert_v59_side_chat(
    connection: &Connection,
    provider_metadata_mode: &str,
    provider_api_mode: &str,
) -> (String, String, String) {
    let project_id = crate::session::ProjectId::new().to_string();
    let owner_id = crate::session::SessionId::new().to_string();
    let conversation_id = crate::session::SessionId::new().to_string();
    let binding_id = crate::storage::SideChatId::new().to_string();
    connection
        .execute(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, 'v60', 'none', 1, 1)",
            params![project_id, format!("C:/v60/{project_id}")],
        )
        .expect("V60 fixture project");
    for session_id in [&owner_id, &conversation_id] {
        connection
            .execute(
                "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      access_mode, model_parameters_json,
                      created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES (?1, ?2, 'session', 'idle', 'C:/v60', 'qwen',
                             'http://localhost:8119/v1', 'default', '{}', 1, 1, NULL)",
                params![session_id, project_id],
            )
            .expect("V60 fixture session");
    }
    connection
        .execute(
            "INSERT INTO side_chat_bindings (
                     id, owner_session_id, conversation_session_id,
                     base_url, model, provider_metadata_mode, provider_api_mode,
                     context_window, max_output_tokens,
                     request_timeout_ms, connect_timeout_ms, max_retries,
                     supports_images, supports_tools, supports_reasoning,
                     persisted_draft, draft_revision, request_generation, context_scope,
                     created_at_ms, updated_at_ms
                 ) VALUES (
                     ?1, ?2, ?3, 'http://localhost:8119/v1', 'qwen', ?4, ?5,
                     65536, 8192, 60000, 5000, 1,
                     0, 1, 1, '', 0, 0, 'general', 2, 2
                 )",
            params![
                binding_id,
                owner_id,
                conversation_id,
                provider_metadata_mode,
                provider_api_mode,
            ],
        )
        .expect("V59 side chat fixture");
    (owner_id, conversation_id, binding_id)
}

fn insert_v58_running_root(connection: &Connection) -> (String, String, String, i64) {
    let session_id = insert_v56_session(connection);
    let admission_id = crate::session::AdmissionId::new().to_string();
    let turn_id = crate::protocol::TurnId::new().to_string();
    connection
        .execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
                 VALUES (?1, ?2, 1)",
            params![session_id, turn_id],
        )
        .expect("V58 root turn allocator");
    let revision = connection
        .query_row(
            "SELECT revision FROM session_admission_revisions WHERE session_id = ?1",
            [&session_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("V58 root revision");
    connection
        .execute(
            "UPDATE sessions
                 SET status = 'running', active_run_id = ?2, active_turn_id = ?3,
                     active_run_lease_expires_at_ms = 100, updated_at_ms = 2
                 WHERE id = ?1",
            params![session_id, admission_id, turn_id],
        )
        .expect("V58 running root");
    (session_id, admission_id, turn_id, revision)
}

fn insert_v58_exact_execution_interrupt_request(
    connection: &Connection,
    session_id: &str,
    admission_id: &str,
    turn_id: &str,
    revision: i64,
    cause: &str,
) {
    connection
        .execute(
            "INSERT INTO exact_execution_interrupt_requests
                 (session_id, admission_id, turn_id, admission_revision, cause, requested_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 3)",
            params![session_id, admission_id, turn_id, revision, cause],
        )
        .expect("V58 exact execution interrupt request");
}

fn insert_v58_raw_completed_terminal(
    connection: &Connection,
    session_id: &str,
    turn_id: &str,
) -> rusqlite::Result<usize> {
    let terminal = crate::protocol::RuntimeEventMsg::TurnTerminal {
        terminal: Box::new(crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    let msg_json = serde_json::to_string(&terminal).expect("V58 terminal JSON");
    connection.execute(
        "INSERT INTO protocol_runtime_events
             (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, 4)",
        params![
            crate::protocol::RuntimeEventId::new().to_string(),
            session_id,
            turn_id,
            msg_json,
            sha256_text(&msg_json),
        ],
    )
}

fn insert_v56_exact_terminalized_orphan(
    connection: &Connection,
    run_id: &str,
    session_id: &str,
    turn_id: &str,
    terminal_event_id: &str,
    with_all_children: bool,
) {
    connection
        .execute(
            "INSERT INTO harness_runs
                 (id, session_id, workspace_root, artifact_root, mode,
                  started_at_ms, completed_at_ms, status, protocol_turn_id,
                  canonical_terminal_runtime_event_id)
                 VALUES (?1, ?2, 'C:/v56', 'C:/v56/artifacts', 'native_runtime',
                         10, 20, '\"pass\"', ?3, ?4)",
            params![run_id, session_id, turn_id, terminal_event_id],
        )
        .expect("V56 orphan run");
    let terminalized_id = crate::harness::HarnessEventId::new().to_string();
    let payload_json = serde_json::json!({
        "type": "generic",
        "data": {
            "kind": "turn_terminal",
            "session_id": session_id,
            "terminal": {
                "outcome": { "kind": "completed" }
            }
        }
    })
    .to_string();
    let payload_sha256 = crate::harness::event_store::event_envelope_sha256(
        &terminalized_id,
        run_id,
        0,
        "\"run_terminalized\"",
        &payload_json,
        "[]",
        "[]",
        None,
        20,
    )
    .expect("V56 terminalized harness event envelope hash");
    connection
        .execute(
            "INSERT INTO harness_events
                 (id, run_id, sequence_no, kind, payload_json,
                  contract_refs_json, artifact_refs_json, parent_event_id,
                  payload_sha256, created_at_ms)
                 VALUES (?1, ?2, 0, '\"run_terminalized\"', ?3,
                         '[]', '[]', NULL, ?4, 20)",
            params![terminalized_id, run_id, payload_json, payload_sha256],
        )
        .expect("V56 terminalized harness event");
    if !with_all_children {
        return;
    }
    connection
        .execute(
            "INSERT INTO harness_artifacts
                 (id, run_id, kind, relative_path, sha256, size_bytes, tags_json,
                  created_by_event_id, contract_refs_json, created_at_ms)
                 VALUES (?1, ?2, 'log', 'run.log', 'sha', 1, '[]', ?3, '[]', 20)",
            params![
                crate::harness::ArtifactId::new().to_string(),
                run_id,
                terminalized_id
            ],
        )
        .expect("V56 harness artifact");
    connection
        .execute(
            "INSERT INTO harness_contracts
                 (run_id, contract_id, kind, version, source_path, content_sha256,
                  schema_ref, model_visible_summary)
                 VALUES (?1, 'contract', 'test', '1', 'contract.md', 'sha', NULL, NULL)",
            [run_id],
        )
        .expect("V56 harness contract");
    connection
        .execute(
            "INSERT INTO harness_gate_results
                 (id, run_id, sequence_no, gate_kind, status, severity, owner,
                  summary, payload_json, created_at_ms)
                 VALUES (?1, ?2, 0, 'test', 'pass', 'info', NULL, 'ok', '{}', 20)",
            params![crate::harness::GateId::new().to_string(), run_id],
        )
        .expect("V56 harness gate");
    connection
        .execute(
            "INSERT INTO harness_replay_reports
                 (run_id, schema_version, status, primary_owner, summary,
                  restart_point, next_actions_json, report_json, created_at_ms)
                 VALUES (?1, '1', 'pass', NULL, 'ok', NULL, '[]', '{}', 20)",
            [run_id],
        )
        .expect("V56 harness replay report");
}

fn insert_v51_history_fixture(
    connection: &Connection,
    id: &str,
    session_id: &str,
    turn_id: &str,
    sequence_no: i64,
    created_at_ms: i64,
    payload_json: &str,
) {
    connection
        .execute(
            "INSERT INTO protocol_history_items (
                     id, session_id, scope_kind, turn_id, sequence_no,
                     payload_json, payload_sha256, created_at_ms
                 )
                 VALUES (?1, ?2, 'turn', ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                session_id,
                turn_id,
                sequence_no,
                payload_json,
                sha256_text(payload_json),
                created_at_ms,
            ],
        )
        .expect("V51 history fixture");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order (
                     session_id, scope_kind, turn_id, sequence_no,
                     source_kind, source_id, created_at_ms
                 )
                 VALUES (?1, 'turn', ?2, ?3, 'history_item', ?4, ?5)",
            params![session_id, turn_id, sequence_no, id, created_at_ms,],
        )
        .expect("V51 append-order fixture");
}

fn exact_v50_with_one_legacy_steer() -> (Connection, String, String, String, String) {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v50_endpoint(&connection);
    let session_id = crate::session::SessionId::new().to_string();
    let turn_id = crate::protocol::TurnId::new();
    let input_id = crate::protocol::HistoryItemId::new().to_string();
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('v51-project', 'C:/v51', 'v51', 'none', 1, 1);",
        )
        .expect("V51 project");
    connection
        .execute(
            "INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES (?1, 'v51-project', 'session', 'idle', 'C:/v51',
                         'model', 'http://localhost', 1, 1, NULL)",
            [&session_id],
        )
        .expect("V51 session");
    let payload_json = serde_json::to_string(&crate::protocol::HistoryItemPayload::SteerTurn {
        expected_turn_id: turn_id,
        content: vec![crate::protocol::ContentPart::Text {
            text: "legacy exact steer".to_string(),
        }],
        additional_context: Default::default(),
        client_user_message_id: Some("legacy-client-id".to_string()),
    })
    .expect("typed legacy steer");
    insert_v51_history_fixture(
        &connection,
        &input_id,
        &session_id,
        &turn_id.to_string(),
        0,
        2,
        &payload_json,
    );
    (
        connection,
        session_id,
        turn_id.to_string(),
        input_id,
        payload_json,
    )
}

fn current_v51_with_one_queued_runtime_steer()
-> (Connection, String, String, String, String, String) {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh current schema");
    let session_id = crate::session::SessionId::new().to_string();
    let admission_id = crate::session::AdmissionId::new().to_string();
    let turn_id = crate::protocol::TurnId::new();
    let input_id = crate::protocol::HistoryItemId::new().to_string();
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('v51-runtime-project', 'C:/v51-runtime', 'v51-runtime', 'none', 1, 1);",
        )
        .expect("V51 runtime project");
    connection
        .execute(
            "INSERT INTO sessions (
                     id, project_id, title, status, cwd_path, model_name, base_url,
                     created_at_ms, updated_at_ms, completed_at_ms,
                     active_run_id, active_turn_id, active_run_lease_expires_at_ms
                 )
                 VALUES (
                     ?1, 'v51-runtime-project', 'session', 'running',
                     'C:/v51-runtime', 'model', 'http://localhost',
                     1, 100, NULL, ?2, ?3, 1000
                 )",
            params![session_id, admission_id, turn_id.to_string()],
        )
        .expect("V51 running session");
    let payload_json = serde_json::to_string(&crate::protocol::HistoryItemPayload::SteerTurn {
        expected_turn_id: turn_id,
        content: vec![crate::protocol::ContentPart::Text {
            text: "queued runtime steer".to_string(),
        }],
        additional_context: Default::default(),
        client_user_message_id: Some("runtime-client-id".to_string()),
    })
    .expect("typed runtime steer");
    connection
        .execute(
            "INSERT INTO turn_steer_inputs (
                     id, session_id, admission_id, turn_id, payload_json,
                     payload_sha256, origin_kind, state,
                     delivered_history_item_id, resolved_by_terminal_event_id,
                     accepted_at_ms, delivered_at_ms, discarded_at_ms, updated_at_ms
                 )
                 VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, 'runtime', 'queued',
                     NULL, NULL, 100, NULL, NULL, 100
                 )",
            params![
                input_id,
                session_id,
                admission_id,
                turn_id.to_string(),
                payload_json,
                sha256_text(&payload_json),
            ],
        )
        .expect("V51 runtime queue");
    validate_durable_turn_input_queue_data(&connection).expect("valid runtime queue");
    (
        connection,
        session_id,
        admission_id,
        turn_id.to_string(),
        input_id,
        payload_json,
    )
}

#[test]
fn v51_backfills_legacy_steer_as_exact_delivered_provenance_without_queue_order() {
    let (connection, session_id, turn_id, input_id, payload_json) =
        exact_v50_with_one_legacy_steer();

    run(&connection).expect("V50 to V51 upgrade");
    run(&connection).expect("idempotent V51 reopen");

    let stored = connection
        .query_row(
            "SELECT session_id, admission_id, turn_id, payload_json,
                        payload_sha256, origin_kind, state,
                        delivered_history_item_id, accepted_at_ms,
                        delivered_at_ms, updated_at_ms
                 FROM turn_steer_inputs
                 WHERE id = ?1",
            [&input_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )
        .expect("backfilled legacy steer");
    assert_eq!(stored.0, session_id);
    assert_eq!(stored.1, None);
    assert_eq!(stored.2, turn_id);
    assert_eq!(stored.3, payload_json);
    assert_eq!(stored.4, sha256_text(&payload_json));
    assert_eq!(stored.5, "legacy_history");
    assert_eq!(stored.6, "delivered");
    assert_eq!(stored.7.as_deref(), Some(input_id.as_str()));
    assert_eq!((stored.8, stored.9, stored.10), (2, Some(2), 2));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM turn_steer_input_enqueue_order",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("no invented legacy FIFO position"),
        0
    );
    validate_durable_turn_input_queue_schema(&connection).expect("exact V51 schema");
    validate_durable_turn_input_queue_data(&connection).expect("valid V51 data");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            },)
            .expect("V51 foreign-key check"),
        0
    );
}

#[test]
fn v51_fast_path_rejects_schema_marker_and_payload_hash_tampering() {
    let schema_connection = Connection::open_in_memory().expect("database");
    run(&schema_connection).expect("fresh current schema");
    schema_connection
        .execute_batch("DROP TRIGGER prevent_turn_steer_enqueue_order_update")
        .expect("drop V51 immutable trigger");
    let schema_error = run(&schema_connection).expect_err("stale V51 schema must fail closed");
    assert!(schema_error.to_string().contains("V51 marker"));

    let marker_connection = Connection::open_in_memory().expect("database");
    run(&marker_connection).expect("fresh current schema");
    marker_connection
        .execute(
            "UPDATE moyai_schema_migrations
                 SET name = 'tampered_v51'
                 WHERE version = ?1",
            [DURABLE_TURN_INPUT_QUEUE_VERSION],
        )
        .expect("tamper V51 marker");
    let marker_error = run(&marker_connection).expect_err("wrong V51 marker must fail closed");
    assert!(
        marker_error
            .to_string()
            .contains(DURABLE_TURN_INPUT_QUEUE_NAME)
    );

    let (hash_connection, _, _, input_id, _) = exact_v50_with_one_legacy_steer();
    run(&hash_connection).expect("V51 upgrade");
    let transition_trigger = hash_connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'validate_turn_steer_input_transition_before_update'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("V51 transition trigger");
    hash_connection
        .execute_batch("DROP TRIGGER validate_turn_steer_input_transition_before_update")
        .expect("temporarily bypass immutable update validation");
    hash_connection
        .execute(
            "UPDATE turn_steer_inputs
                 SET payload_sha256 = 'stale'
                 WHERE id = ?1",
            [&input_id],
        )
        .expect("tamper payload hash");
    hash_connection
        .execute_batch(&transition_trigger)
        .expect("restore exact transition trigger");
    let hash_error = run(&hash_connection).expect_err("stale V51 payload hash must fail closed");
    assert!(
        hash_error.to_string().contains("V51 marker")
            && hash_error.to_string().contains("turn-steer input"),
        "unexpected V51 hash audit error: {hash_error}"
    );
}

#[test]
fn v52_preserves_legacy_harness_rows_without_inventing_turn_identity() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v51_endpoint(&connection);
    let run_id = crate::harness::HarnessRunId::new().to_string();
    let event_id = crate::harness::HarnessEventId::new().to_string();
    let artifact_id = crate::harness::ArtifactId::new().to_string();
    connection
        .execute(
            "INSERT INTO harness_runs
                 (id, session_id, workspace_root, artifact_root, mode,
                  started_at_ms, completed_at_ms, status)
                 VALUES (?1, NULL, 'C:/workspace', 'C:/artifact',
                         'legacy_replay', 1, 2, '\"pass\"')",
            [&run_id],
        )
        .expect("legacy harness run");
    connection
        .execute(
            "INSERT INTO harness_events
                 (id, run_id, sequence_no, kind, payload_json,
                  contract_refs_json, artifact_refs_json, parent_event_id,
                  payload_sha256, created_at_ms)
                 VALUES (?1, ?2, 0, '\"quality_gate_evaluated\"',
                         '{\"type\":\"generic\",\"data\":null}',
                         '[]', '[]', NULL, 'legacy-hash', 2)",
            params![event_id, run_id],
        )
        .expect("legacy harness event");
    connection
        .execute(
            "INSERT INTO harness_artifacts
                 (id, run_id, kind, relative_path, sha256, size_bytes,
                  tags_json, created_by_event_id, contract_refs_json,
                  created_at_ms)
                 VALUES (?1, ?2, '\"verification_log\"', 'legacy.json',
                         'artifact-hash', 1, '[]', ?3, '[]', 2)",
            params![artifact_id, run_id, event_id],
        )
        .expect("legacy harness artifact");

    run(&connection).expect("V51 to V52 upgrade");
    run(&connection).expect("idempotent V52 reopen");

    let retained = connection
        .query_row(
            "SELECT protocol_turn_id,
                        canonical_terminal_runtime_event_id,
                        status
                 FROM harness_runs WHERE id = ?1",
            [&run_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .expect("retained harness run");
    assert_eq!(retained, (None, None, "\"pass\"".to_string()));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM harness_events WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("retained events"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM harness_artifacts WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("retained artifacts"),
        1
    );
    validate_harness_turn_identity_schema(&connection).expect("exact V52 schema");
    validate_harness_turn_identity_data(&connection).expect("valid V52 data");
    assert!(
        schema_migration_has_exact_name(
            &connection,
            HARNESS_TURN_IDENTITY_VERSION,
            HARNESS_TURN_IDENTITY_NAME,
        )
        .expect("V52 marker")
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("V52 foreign-key check"),
        0
    );
}

#[test]
fn v52_fast_path_rejects_schema_marker_and_linkage_tampering() {
    let schema_connection = Connection::open_in_memory().expect("database");
    run(&schema_connection).expect("fresh current schema");
    schema_connection
        .execute_batch("DROP INDEX idx_harness_events_unique_run_terminal")
        .expect("drop V52 terminal index");
    let schema_error = run(&schema_connection).expect_err("stale V52 schema must fail closed");
    assert!(schema_error.to_string().contains("V52 marker"));

    let marker_connection = Connection::open_in_memory().expect("database");
    run(&marker_connection).expect("fresh current schema");
    marker_connection
        .execute(
            "UPDATE moyai_schema_migrations
                 SET name = 'tampered_v52'
                 WHERE version = ?1",
            [HARNESS_TURN_IDENTITY_VERSION],
        )
        .expect("tamper V52 marker");
    let marker_error = run(&marker_connection).expect_err("wrong V52 marker must fail closed");
    assert!(
        marker_error
            .to_string()
            .contains(HARNESS_TURN_IDENTITY_NAME)
    );

    let linkage_connection = Connection::open_in_memory().expect("database");
    run(&linkage_connection).expect("fresh current schema");
    linkage_connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for corruption fixture");
    linkage_connection
        .execute(
            "INSERT INTO harness_runs
                 (id, session_id, protocol_turn_id,
                  canonical_terminal_runtime_event_id,
                  workspace_root, artifact_root, mode, started_at_ms,
                  completed_at_ms, status)
                 VALUES (?1, ?2, ?3, ?4, 'C:/workspace', 'C:/artifact',
                         'native_runtime', 1, 2, '\"pass\"')",
            params![
                crate::harness::HarnessRunId::new().to_string(),
                crate::session::SessionId::new().to_string(),
                crate::protocol::TurnId::new().to_string(),
                crate::protocol::RuntimeEventId::new().to_string(),
            ],
        )
        .expect("corrupt V52 linkage");
    linkage_connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
    let linkage_error = run(&linkage_connection).expect_err("orphan linkage must fail closed");
    assert!(
        linkage_error
            .to_string()
            .contains("canonical terminal linkage"),
        "unexpected V52 linkage audit error: {linkage_error}"
    );
}

#[test]
fn v56_valid_v55_is_noop_preserves_legacy_unmapped_and_reopens() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&connection);
    let session_id = insert_v56_session(&connection);
    let legacy_run_id = crate::harness::HarnessRunId::new().to_string();
    connection
        .execute(
            "INSERT INTO harness_runs
                 (id, session_id, workspace_root, artifact_root, mode,
                  started_at_ms, completed_at_ms, status, protocol_turn_id,
                  canonical_terminal_runtime_event_id)
                 VALUES (?1, ?2, 'C:/v56', 'C:/v56/artifacts', 'manual',
                         10, 20, '\"pass\"', NULL, NULL)",
            params![legacy_run_id, session_id],
        )
        .expect("legacy unmapped harness run");

    run(&connection).expect("V55 to V56 no-op upgrade");
    run(&connection).expect("V56 idempotent reopen");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            ROLLBACK_HARNESS_ORPHAN_RECOVERY_VERSION,
            ROLLBACK_HARNESS_ORPHAN_RECOVERY_NAME,
        )
        .expect("V56 marker")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM harness_runs WHERE id = ?1",
                [&legacy_run_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("legacy run count"),
        1
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v57_backfills_allocator_count_and_ignores_session_scoped_history_turn_null() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v56_endpoint(&connection);
    let session_id = insert_v56_session(&connection);
    let turn_a = crate::protocol::TurnId::new().to_string();
    let turn_b = crate::protocol::TurnId::new().to_string();
    connection
        .execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
                 VALUES (?1, ?2, 1), (?1, ?3, 1)",
            params![session_id, turn_a, turn_b],
        )
        .expect("two legacy turn allocators");

    let history_id = crate::protocol::HistoryItemId::new().to_string();
    let payload_json = serde_json::to_string(
        &crate::protocol::HistoryItemPayload::CollaborationModeInstruction {
            mode: crate::agent::mode::ModeKind::Plan,
        },
    )
    .expect("session-scoped payload");
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no,
                  payload_json, payload_sha256, created_at_ms)
                 VALUES (?1, ?2, 'session', NULL, 0, ?3, ?4, 2)",
            params![
                history_id,
                session_id,
                payload_json,
                sha256_text(&payload_json),
            ],
        )
        .expect("V42 session-scoped history fixture");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES (?1, 'session', NULL, 0, 'history_item', ?2, 2)",
            params![session_id, history_id],
        )
        .expect("session-scoped append order");

    run(&connection).expect("V56 to V57 upgrade");
    assert_eq!(
        connection
            .query_row(
                "SELECT revision FROM session_admission_revisions WHERE session_id = ?1",
                [&session_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("V57 baseline revision"),
        2,
        "only the two canonical turn allocators contribute to the baseline"
    );
    run(&connection).expect("V57 idempotent reopen");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v57_reopen_rejects_revision_below_current_allocator_count() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh V57 schema");
    let session_id = insert_v56_session(&connection);
    connection
        .execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
                 VALUES (?1, ?2, 1), (?1, ?3, 1)",
            params![
                session_id,
                crate::protocol::TurnId::new().to_string(),
                crate::protocol::TurnId::new().to_string(),
            ],
        )
        .expect("two current turn allocators");
    connection
        .execute(
            "UPDATE session_admission_revisions SET revision = 1 WHERE session_id = ?1",
            [&session_id],
        )
        .expect("inject regressed revision");

    let error = run(&connection).expect_err("regressed admission revision must fail closed");
    assert!(
        error.to_string().contains("lack a valid revision"),
        "unexpected V57 data-audit error: {error}"
    );
}

#[test]
fn v58_upgrades_v57_reopens_pending_request_and_cascades_with_parent_session() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v57_endpoint(&connection);
    run_to_current(&connection).expect("V57 to V58 current-endpoint upgrade");
    assert!(
        schema_migration_has_exact_name(
            &connection,
            EXACT_EXECUTION_INTERRUPT_REQUESTS_VERSION,
            EXACT_EXECUTION_INTERRUPT_REQUESTS_NAME,
        )
        .expect("V58 marker")
    );
    let (session_id, admission_id, turn_id, revision) = insert_v58_running_root(&connection);
    insert_v58_exact_execution_interrupt_request(
        &connection,
        &session_id,
        &admission_id,
        &turn_id,
        revision,
        "user_stop",
    );

    run(&connection).expect("V58 pending request reopen");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM exact_execution_interrupt_requests WHERE session_id = ?1",
                [&session_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("pending request count"),
        1
    );
    let raw_terminal_error = insert_v58_raw_completed_terminal(&connection, &session_id, &turn_id)
        .expect_err("raw terminal must not bypass a pending exact interrupt request");
    assert!(
        raw_terminal_error
            .to_string()
            .contains("must be consumed before TurnTerminal"),
        "unexpected raw-terminal guard error: {raw_terminal_error}"
    );
    connection
        .execute(
            "UPDATE sessions
                 SET status = 'completed', completed_at_ms = 4,
                     active_run_id = NULL, active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1 AND status = 'running'",
            [&session_id],
        )
        .expect("canonical status CAS consumes pending request");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM exact_execution_interrupt_requests WHERE session_id = ?1",
                [&session_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("terminal-consumed request count"),
        0
    );
    insert_v58_raw_completed_terminal(&connection, &session_id, &turn_id)
        .expect("terminal insert succeeds only after request consumption");

    let (rollback_session_id, rollback_admission_id, rollback_turn_id, rollback_revision) =
        insert_v58_running_root(&connection);
    insert_v58_exact_execution_interrupt_request(
        &connection,
        &rollback_session_id,
        &rollback_admission_id,
        &rollback_turn_id,
        rollback_revision,
        "user_stop",
    );
    connection
        .execute(
            "DELETE FROM protocol_turn_sequence_allocators
                 WHERE session_id = ?1 AND turn_id = ?2",
            params![rollback_session_id, rollback_turn_id],
        )
        .expect("rollback-style exact turn cleanup");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM exact_execution_interrupt_requests WHERE session_id = ?1",
                [&rollback_session_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("turn cleanup request count"),
        0,
        "the rollback-owned allocator delete must consume its exact request"
    );
    connection
        .execute("DELETE FROM sessions WHERE id = ?1", [&rollback_session_id])
        .expect("delete turn-cleanup fixture");

    let (cascade_session_id, cascade_admission_id, cascade_turn_id, cascade_revision) =
        insert_v58_running_root(&connection);
    insert_v58_exact_execution_interrupt_request(
        &connection,
        &cascade_session_id,
        &cascade_admission_id,
        &cascade_turn_id,
        cascade_revision,
        "user_stop",
    );
    connection
        .execute("DELETE FROM sessions WHERE id = ?1", [&cascade_session_id])
        .expect("parent session cascade");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM exact_execution_interrupt_requests",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .expect("request cascade count"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM session_admission_revisions WHERE session_id = ?1",
                [&cascade_session_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("revision cascade count"),
        0,
        "V57 turn-delete revision trigger must not abort a parent session cascade"
    );
    connection
        .execute(
            "DELETE FROM protocol_turn_sequence_allocators WHERE session_id = ?1",
            [&cascade_session_id],
        )
        .expect("clean allocator without a legacy parent foreign key");
    assert!(foreign_key_violations(&connection).is_empty());
    run(&connection).expect("V58 reopen after cascade");
}

#[test]
fn v58_fast_path_rejects_schema_and_exact_owner_corruption() {
    let schema_connection = Connection::open_in_memory().expect("schema database");
    run(&schema_connection).expect("fresh V58 schema");
    schema_connection
        .execute_batch("DROP TRIGGER exact_execution_interrupt_requests_prevent_update")
        .expect("drop V58 trigger");
    let schema_error = run(&schema_connection).expect_err("stale V58 schema must fail closed");
    assert!(
        schema_error.to_string().contains("V58 marker"),
        "unexpected V58 schema error: {schema_error}"
    );

    let data_connection = Connection::open_in_memory().expect("data database");
    data_connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&data_connection).expect("fresh V58 data schema");
    let (session_id, admission_id, turn_id, revision) = insert_v58_running_root(&data_connection);
    insert_v58_exact_execution_interrupt_request(
        &data_connection,
        &session_id,
        &admission_id,
        &turn_id,
        revision,
        "user_stop",
    );
    let immutable_trigger = data_connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'exact_execution_interrupt_requests_prevent_update'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("immutable trigger SQL");
    data_connection
        .execute_batch("DROP TRIGGER exact_execution_interrupt_requests_prevent_update")
        .expect("temporarily drop immutable trigger");
    data_connection
        .execute(
            "UPDATE exact_execution_interrupt_requests
                 SET admission_revision = admission_revision + 1
                 WHERE session_id = ?1",
            [&session_id],
        )
        .expect("inject stale request revision");
    data_connection
        .execute_batch(&immutable_trigger)
        .expect("restore immutable trigger");
    let data_error = run(&data_connection).expect_err("stale V58 owner must fail closed on reopen");
    assert!(
        data_error
            .to_string()
            .contains("exact execution interrupt request row"),
        "unexpected V58 data error: {data_error}"
    );
}

#[test]
fn v58_fast_path_rejects_noncanonical_lowercase_interrupt_identity() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh V58 schema");
    let (session_id, admission_id, turn_id, revision) = insert_v58_running_root(&connection);
    insert_v58_exact_execution_interrupt_request(
        &connection,
        &session_id,
        &admission_id,
        &turn_id,
        revision,
        "user_stop",
    );
    let immutable_trigger = connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'exact_execution_interrupt_requests_prevent_update'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("immutable trigger SQL");
    connection
        .execute_batch("DROP TRIGGER exact_execution_interrupt_requests_prevent_update")
        .expect("temporarily drop immutable trigger");
    let lowercase_admission_id = admission_id.to_ascii_lowercase();
    connection
        .execute(
            "UPDATE sessions SET active_run_id = ?2 WHERE id = ?1",
            params![session_id, lowercase_admission_id],
        )
        .expect("corrupt canonical active admission spelling");
    connection
        .execute(
            "UPDATE exact_execution_interrupt_requests
                 SET admission_id = ?2
                 WHERE session_id = ?1",
            params![session_id, lowercase_admission_id],
        )
        .expect("corrupt canonical request admission spelling");
    connection
        .execute_batch(&immutable_trigger)
        .expect("restore immutable trigger");

    let error = run(&connection)
        .expect_err("noncanonical lowercase V58 identity must fail closed on reopen");
    assert!(
        error
            .to_string()
            .contains("exact execution interrupt request row"),
        "unexpected V58 identity error: {error}"
    );
}

#[test]
fn v58_insert_trigger_rejects_terminalized_or_wrong_topology_request() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh V58 schema");

    let (terminal_session, terminal_admission, terminal_turn, terminal_revision) =
        insert_v58_running_root(&connection);
    insert_v58_raw_completed_terminal(&connection, &terminal_session, &terminal_turn)
        .expect("preexisting raw terminal fixture");
    let terminal_error = connection
        .execute(
            "INSERT INTO exact_execution_interrupt_requests
                 (session_id, admission_id, turn_id, admission_revision, cause, requested_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'user_stop', 5)",
            params![
                terminal_session,
                terminal_admission,
                terminal_turn,
                terminal_revision,
            ],
        )
        .expect_err("a preexisting exact terminal rejects the request");
    assert!(terminal_error.to_string().contains("live exact admission"));

    let (root_session, root_admission, root_turn, root_revision) =
        insert_v58_running_root(&connection);
    let topology_error = connection
        .execute(
            "INSERT INTO exact_execution_interrupt_requests
                 (session_id, admission_id, turn_id, admission_revision, cause, requested_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'agent_interrupted', 5)",
            params![root_session, root_admission, root_turn, root_revision],
        )
        .expect_err("AgentInterrupted requires a child topology");
    assert!(topology_error.to_string().contains("matching topology"));
}

#[test]
fn v58_fast_path_rejects_pending_request_with_terminal_corruption() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh V58 schema");
    let (session_id, admission_id, turn_id, revision) = insert_v58_running_root(&connection);
    insert_v58_exact_execution_interrupt_request(
        &connection,
        &session_id,
        &admission_id,
        &turn_id,
        revision,
        "user_stop",
    );
    let guard_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'exact_execution_interrupt_requests_reject_raw_terminal'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("raw terminal guard SQL");
    connection
        .execute_batch("DROP TRIGGER exact_execution_interrupt_requests_reject_raw_terminal")
        .expect("drop raw terminal guard for corruption fixture");
    insert_v58_raw_completed_terminal(&connection, &session_id, &turn_id)
        .expect("inject terminal behind pending request");
    connection
        .execute_batch(&guard_sql)
        .expect("restore raw terminal guard");

    let error = run(&connection).expect_err("request plus terminal must fail closed on reopen");
    assert!(
        error
            .to_string()
            .contains("exact execution interrupt request row"),
        "unexpected V58 terminal-corruption error: {error}"
    );
}

#[test]
fn v56_recovers_run28_terminalized_and_mapped_started_shapes_then_reopens() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&connection);
    let session_id = "01M0A6DRJCQRVRFN97J7H39S6Y";
    let turn_id = "01M0A6DRQJY1MP90K1RG0RXQ2T";
    let run_ids = ["01M0A6DRQMARWG1Z30HDS2AKKC", "01M0A6DRQMARWG1Z30HDS2AKKD"];
    let terminal_ids = ["01M0A6DRRP4PJNDF3G4WP8CV3K", "01M0A6DRRP4PJNDF3G4WP8CV3M"];
    insert_v56_session_with_id(&connection, session_id);
    for (index, run_id) in run_ids.iter().enumerate() {
        insert_v56_exact_terminalized_orphan(
            &connection,
            run_id,
            session_id,
            turn_id,
            terminal_ids[index],
            index == 1,
        );
    }
    let run30_event_id = "01M0A6DRRQJY5BJQ7TBKBHWJYQ";
    let run30_payload = r#"{"type":"generic","data":{"kind":"turn_terminal","session_id":"01M0A6DRJCQRVRFN97J7H39S6Y","terminal":{"change_count":0,"failed_tool_count":0,"final_response_id":"01M0A6DRR4GKSMKMS8TZ21QHCE","metrics":{"config":{"access_mode":"full_access","base_url":"http://127.0.0.1:9815","model":"uat/run28-persistence"},"elapsed_ms":27,"model_request_count":1,"token_usage":{"completion_tokens":6,"prompt_tokens":12,"reasoning_tokens":0,"total_tokens":18}},"outcome":{"kind":"completed"},"tool_call_count":0}}}"#;
    let run30_completed_at_ms = 1_787_048_682_262_i64;
    let run30_envelope_sha256 = crate::harness::event_store::event_envelope_sha256(
        run30_event_id,
        run_ids[0],
        12,
        "\"run_terminalized\"",
        run30_payload,
        "[]",
        "[]",
        None,
        run30_completed_at_ms,
    )
    .expect("Run30 terminalized event envelope hash");
    assert_eq!(
        run30_envelope_sha256,
        "envelope-v1:6e4a3303a44b574f90bf5f922e5cf48b7ef15d659af64d11d241a38a199caba6",
        "regression fixture must remain byte-exact with the frozen Run30 event"
    );
    connection
        .execute(
            "UPDATE harness_events
                 SET id = ?2, sequence_no = 12, payload_json = ?3,
                     payload_sha256 = ?4, created_at_ms = ?5
                 WHERE run_id = ?1",
            params![
                run_ids[0],
                run30_event_id,
                run30_payload,
                run30_envelope_sha256,
                run30_completed_at_ms,
            ],
        )
        .expect("replace first fixture with exact Run30 terminalized event");
    connection
        .execute(
            "UPDATE harness_runs SET completed_at_ms = ?2 WHERE id = ?1",
            params![run_ids[0], run30_completed_at_ms],
        )
        .expect("match exact Run30 terminal completion time");

    let legacy_payload = connection
        .query_row(
            "SELECT payload_json FROM harness_events WHERE run_id = ?1",
            [run_ids[1]],
            |row| row.get::<_, String>(0),
        )
        .expect("legacy terminalized event payload");
    connection
        .execute(
            "UPDATE harness_events SET payload_sha256 = ?2 WHERE run_id = ?1",
            params![run_ids[1], sha256_text(&legacy_payload)],
        )
        .expect("retain V14 payload-only hash compatibility fixture");
    let started_run_id = "01M0A6DRQMARWG1Z30HDS2AKKE";
    connection
        .execute(
            "INSERT INTO harness_runs
                 (id, session_id, workspace_root, artifact_root, mode,
                  started_at_ms, completed_at_ms, status, protocol_turn_id,
                  canonical_terminal_runtime_event_id)
                 VALUES (?1, ?2, 'C:/v56', 'C:/v56/artifacts', 'native_runtime',
                         10, NULL, '\"started\"', ?3, NULL)",
            params![started_run_id, session_id, turn_id],
        )
        .expect("mapped started rollback orphan");

    run(&connection).expect("recover exact Run28 orphan shape");
    run(&connection).expect("reopen recovered V56 database");

    for table_name in [
        "harness_replay_reports",
        "harness_gate_results",
        "harness_contracts",
        "harness_artifacts",
        "harness_events",
        "harness_runs",
    ] {
        let remaining = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table_name} WHERE run_id IN (?1, ?2)")
                    .replace("harness_runs WHERE run_id", "harness_runs WHERE id"),
                params![run_ids[0], run_ids[1]],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_else(|error| panic!("{table_name} recovery count: {error}"));
        assert_eq!(remaining, 0, "{table_name} must be removed child-first");
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM harness_runs WHERE id = ?1",
                [started_run_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("started orphan recovery count"),
        0,
        "mapped started rollback orphan must also be removed"
    );
    assert!(
        schema_migration_has_exact_name(
            &connection,
            ROLLBACK_HARNESS_ORPHAN_RECOVERY_VERSION,
            ROLLBACK_HARNESS_ORPHAN_RECOVERY_NAME,
        )
        .expect("V56 marker")
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v56_non_exact_orphans_fail_closed_and_keep_marker_atomic() {
    fn assert_not_migrated(connection: &Connection, run_id: &str) {
        assert!(
            !schema_migration_applied(connection, ROLLBACK_HARNESS_ORPHAN_RECOVERY_VERSION)
                .expect("V56 marker query"),
            "failed recovery must not commit its marker"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM harness_runs WHERE id = ?1",
                    [run_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("orphan run count"),
            1,
            "failed recovery must roll back candidate deletion"
        );
    }

    let partial = Connection::open_in_memory().expect("partial database");
    partial
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&partial);
    let partial_session = insert_v56_session(&partial);
    let partial_turn = crate::protocol::TurnId::new().to_string();
    let partial_run = crate::harness::HarnessRunId::new().to_string();
    insert_v56_exact_terminalized_orphan(
        &partial,
        &partial_run,
        &partial_session,
        &partial_turn,
        &crate::protocol::RuntimeEventId::new().to_string(),
        false,
    );
    partial
        .execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no) VALUES (?1, ?2, 0)",
            params![partial_session, partial_turn],
        )
        .expect("partial canonical residue");
    let error = run(&partial).expect_err("partial canonical turn must fail closed");
    assert!(error.to_string().contains("canonical terminal linkage"));
    assert_not_migrated(&partial, &partial_run);

    let cross_session = Connection::open_in_memory().expect("cross-session database");
    cross_session
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&cross_session);
    let owner_session = insert_v56_session(&cross_session);
    let other_session = insert_v56_session(&cross_session);
    let cross_turn = crate::protocol::TurnId::new().to_string();
    let cross_run = crate::harness::HarnessRunId::new().to_string();
    let cross_terminal = crate::protocol::RuntimeEventId::new().to_string();
    insert_v56_exact_terminalized_orphan(
        &cross_session,
        &cross_run,
        &owner_session,
        &cross_turn,
        &cross_terminal,
        false,
    );
    cross_session
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json,
                  payload_sha256, created_at_ms)
                 VALUES (?1, ?2, ?3, 0, '{}', ?4, 20)",
            params![
                cross_terminal,
                other_session,
                crate::protocol::TurnId::new().to_string(),
                sha256_text("{}"),
            ],
        )
        .expect("cross-session terminal residue");
    let error = run(&cross_session).expect_err("cross-session terminal must fail closed");
    assert!(error.to_string().contains("canonical terminal linkage"));
    assert_not_migrated(&cross_session, &cross_run);

    let invalid = Connection::open_in_memory().expect("invalid database");
    invalid
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&invalid);
    let invalid_session = insert_v56_session(&invalid);
    insert_v56_exact_terminalized_orphan(
        &invalid,
        "invalid-run-id",
        &invalid_session,
        &crate::protocol::TurnId::new().to_string(),
        &crate::protocol::RuntimeEventId::new().to_string(),
        false,
    );
    let error = run(&invalid).expect_err("invalid candidate id must fail closed");
    assert!(error.to_string().contains("invalid harness run id"));
    assert_not_migrated(&invalid, "invalid-run-id");

    let stale_hash = Connection::open_in_memory().expect("stale-hash database");
    stale_hash
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&stale_hash);
    let stale_session = insert_v56_session(&stale_hash);
    let stale_run = crate::harness::HarnessRunId::new().to_string();
    insert_v56_exact_terminalized_orphan(
        &stale_hash,
        &stale_run,
        &stale_session,
        &crate::protocol::TurnId::new().to_string(),
        &crate::protocol::RuntimeEventId::new().to_string(),
        false,
    );
    stale_hash
        .execute(
            "UPDATE harness_events SET payload_sha256 = 'stale' WHERE run_id = ?1",
            [&stale_run],
        )
        .expect("stale terminalized payload hash");
    let error = run(&stale_hash).expect_err("stale harness hash must fail closed");
    assert!(
        error
            .to_string()
            .contains("stale terminalized harness payload hash")
    );
    assert_not_migrated(&stale_hash, &stale_run);

    let disguised_legacy = Connection::open_in_memory().expect("disguised-legacy database");
    disguised_legacy
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&disguised_legacy);
    let disguised_session = insert_v56_session(&disguised_legacy);
    let disguised_run = crate::harness::HarnessRunId::new().to_string();
    insert_v56_exact_terminalized_orphan(
        &disguised_legacy,
        &disguised_run,
        &disguised_session,
        &crate::protocol::TurnId::new().to_string(),
        &crate::protocol::RuntimeEventId::new().to_string(),
        false,
    );
    let disguised_payload = disguised_legacy
        .query_row(
            "SELECT payload_json FROM harness_events WHERE run_id = ?1",
            [&disguised_run],
            |row| row.get::<_, String>(0),
        )
        .expect("disguised legacy payload");
    disguised_legacy
        .execute(
            "UPDATE harness_events SET payload_sha256 = ?2 WHERE run_id = ?1",
            params![
                disguised_run,
                format!("envelope-v1:{}", sha256_text(&disguised_payload))
            ],
        )
        .expect("prefix legacy payload hash as current envelope");
    let error = run(&disguised_legacy)
        .expect_err("payload-only digest with a current prefix must fail closed");
    assert!(
        error
            .to_string()
            .contains("stale terminalized harness payload hash")
    );
    assert_not_migrated(&disguised_legacy, &disguised_run);

    let invalid_payload = Connection::open_in_memory().expect("invalid-payload database");
    invalid_payload
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&invalid_payload);
    let payload_session = insert_v56_session(&invalid_payload);
    let payload_run = crate::harness::HarnessRunId::new().to_string();
    insert_v56_exact_terminalized_orphan(
        &invalid_payload,
        &payload_run,
        &payload_session,
        &crate::protocol::TurnId::new().to_string(),
        &crate::protocol::RuntimeEventId::new().to_string(),
        false,
    );
    let malformed_payload = serde_json::json!({
        "type": "generic",
        "data": {
            "kind": "turn_terminal",
            "session_id": payload_session,
            "terminal": {
                "outcome": { "kind": "completed" },
                "unexpected": true
            }
        }
    })
    .to_string();
    invalid_payload
        .execute(
            "UPDATE harness_events
                 SET payload_json = ?2, payload_sha256 = ?3
                 WHERE run_id = ?1",
            params![
                payload_run,
                malformed_payload,
                sha256_text(&malformed_payload)
            ],
        )
        .expect("invalid typed terminalized payload");
    let error = run(&invalid_payload).expect_err("invalid terminal payload must fail closed");
    assert!(
        error
            .to_string()
            .contains("invalid typed terminal run event")
    );
    assert_not_migrated(&invalid_payload, &payload_run);

    let dependency = Connection::open_in_memory().expect("dependency database");
    dependency
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v55_endpoint(&dependency);
    let dependency_session = insert_v56_session(&dependency);
    let dependency_turn = crate::protocol::TurnId::new().to_string();
    let dependency_run = crate::harness::HarnessRunId::new().to_string();
    insert_v56_exact_terminalized_orphan(
        &dependency,
        &dependency_run,
        &dependency_session,
        &dependency_turn,
        &crate::protocol::RuntimeEventId::new().to_string(),
        false,
    );
    dependency
        .execute(
            "UPDATE sessions
                 SET status = 'running', active_run_id = ?2, active_turn_id = ?3,
                     active_run_lease_expires_at_ms = 100
                 WHERE id = ?1",
            params![
                dependency_session,
                crate::session::AdmissionId::new().to_string(),
                dependency_turn,
            ],
        )
        .expect("active turn dependency residue");
    let error = run(&dependency).expect_err("active turn dependency must fail closed");
    assert!(error.to_string().contains("canonical terminal linkage"));
    assert_not_migrated(&dependency, &dependency_run);
}

#[test]
fn v55_upgrades_exact_v54_reopens_idempotently_and_owns_exact_schema() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v51_endpoint(&connection);
    connection
        .execute_batch(V52_HARNESS_TURN_IDENTITY)
        .expect("exact V52 endpoint");
    connection
        .execute_batch(V53_AGENT_TRIGGER_TURN_CLAIMS)
        .expect("exact V53 endpoint");
    connection
        .execute_batch(V54_PERMISSION_RETRY_FENCES)
        .expect("exact V54 endpoint");
    assert!(
        !schema_migration_applied(&connection, DURABLE_SIDE_CHATS_VERSION)
            .expect("V55 marker query")
    );

    run(&connection).expect("V54 to V55 upgrade");
    run(&connection).expect("idempotent V55 reopen");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            DURABLE_SIDE_CHATS_VERSION,
            DURABLE_SIDE_CHATS_NAME,
        )
        .expect("V55 marker")
    );
    validate_provider_connection_profiles_schema(&connection).expect("exact current schema");
    validate_provider_connection_profiles_data(&connection).expect("valid current data");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v55_fast_path_rejects_schema_marker_and_identity_tampering() {
    let schema_connection = Connection::open_in_memory().expect("database");
    run(&schema_connection).expect("fresh current schema");
    schema_connection
        .execute_batch("DROP TRIGGER validate_side_chat_binding_before_update")
        .expect("drop V55 transition trigger");
    let schema_error = run(&schema_connection).expect_err("stale V55 schema must fail closed");
    assert!(
        schema_error.to_string().contains("V55 marker"),
        "unexpected V55 schema audit error: {schema_error}"
    );

    let tombstone_schema_connection = Connection::open_in_memory().expect("database");
    run(&tombstone_schema_connection).expect("fresh current schema");
    tombstone_schema_connection
        .execute_batch("DROP TRIGGER prevent_tombstoned_side_chat_binding_update")
        .expect("drop V55 tombstone freeze trigger");
    let tombstone_schema_error =
        run(&tombstone_schema_connection).expect_err("stale V55 tombstone schema must fail closed");
    assert!(
        tombstone_schema_error.to_string().contains("V55 marker"),
        "unexpected V55 tombstone schema audit error: {tombstone_schema_error}"
    );

    let marker_connection = Connection::open_in_memory().expect("database");
    run(&marker_connection).expect("fresh current schema");
    marker_connection
        .execute(
            "UPDATE moyai_schema_migrations
                 SET name = 'tampered_v55'
                 WHERE version = ?1",
            [DURABLE_SIDE_CHATS_VERSION],
        )
        .expect("tamper V55 marker");
    let marker_error = run(&marker_connection).expect_err("wrong V55 marker must fail closed");
    assert!(
        marker_error.to_string().contains(DURABLE_SIDE_CHATS_NAME),
        "unexpected V55 marker audit error: {marker_error}"
    );

    let data_connection = Connection::open_in_memory().expect("database");
    data_connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&data_connection).expect("fresh current schema");
    let project_id = crate::session::ProjectId::new().to_string();
    let owner_id = crate::session::SessionId::new().to_string();
    let conversation_id = crate::session::SessionId::new().to_string();
    data_connection
        .execute(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES (?1, 'C:/v55', 'v55', 'none', 1, 1)",
            [&project_id],
        )
        .expect("V55 fixture project");
    for session_id in [&owner_id, &conversation_id] {
        data_connection
            .execute(
                "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      access_mode, model_parameters_json,
                      created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES (?1, ?2, 'session', 'idle', 'C:/v55', 'gemma',
                             'http://localhost:1234', 'default', '{}', 1, 1, NULL)",
                params![session_id, project_id],
            )
            .expect("V55 fixture session");
    }
    data_connection
        .execute(
            "INSERT INTO side_chat_bindings (
                     id, owner_session_id, conversation_session_id,
                     base_url, model, provider_metadata_mode, provider_api_mode,
                     context_window, max_output_tokens,
                     request_timeout_ms, connect_timeout_ms, max_retries,
                     supports_images, supports_tools, supports_reasoning,
                     persisted_draft, draft_revision, request_generation, context_scope,
                     created_at_ms, updated_at_ms
                 ) VALUES (
                     '!!!!!!!!!!!!!!!!!!!!!!!!!!', ?1, ?2,
                     'http://localhost:1234', 'gemma',
                     'lm_studio_native_required', 'chat_completions',
                     65536, 8192, 60000, 5000, 1,
                     0, 0, 1, '', 0, 0, 'general', 2, 2
                 )",
            params![owner_id, conversation_id],
        )
        .expect("inject invalid V55 binding identity");
    let data_error = run(&data_connection).expect_err("invalid V55 id must fail closed");
    assert!(
        data_error.to_string().contains("invalid id"),
        "unexpected V55 data audit error: {data_error}"
    );
}

#[test]
fn current_runner_upgrades_exact_v53_through_current_in_one_startup() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v51_endpoint(&connection);
    connection
        .execute_batch(V52_HARNESS_TURN_IDENTITY)
        .expect("exact V52 endpoint");
    connection
        .execute_batch(V53_AGENT_TRIGGER_TURN_CLAIMS)
        .expect("exact V53 endpoint");
    assert!(
        !schema_migration_applied(&connection, PERMISSION_RETRY_FENCES_VERSION)
            .expect("V54 marker query")
    );

    run_to_current(&connection).expect("V53 through current upgrade");
    run_to_current(&connection).expect("idempotent current reopen");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            PERMISSION_RETRY_FENCES_VERSION,
            PERMISSION_RETRY_FENCES_NAME,
        )
        .expect("V54 marker")
    );
    validate_permission_retry_fences_schema(&connection).expect("exact V54 schema");
    validate_permission_retry_fences_data(&connection).expect("valid V54 data");
    assert!(
        schema_migration_has_exact_name(
            &connection,
            DURABLE_SIDE_CHATS_VERSION,
            DURABLE_SIDE_CHATS_NAME,
        )
        .expect("V55 marker")
    );
    assert!(
        schema_migration_has_exact_name(
            &connection,
            PROVIDER_CONNECTION_PROFILES_VERSION,
            PROVIDER_CONNECTION_PROFILES_NAME,
        )
        .expect("V60 marker")
    );
    assert!(
        schema_migration_has_exact_name(
            &connection,
            SIDE_CHAT_SYSTEM_PROMPT_VERSION,
            SIDE_CHAT_SYSTEM_PROMPT_NAME,
        )
        .expect("V61 marker")
    );
    validate_side_chat_system_prompt_schema(&connection).expect("exact current schema");
    validate_side_chat_system_prompt_data(&connection).expect("valid current data");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v54_fast_path_rejects_schema_marker_and_row_tampering() {
    let schema_connection = Connection::open_in_memory().expect("database");
    run(&schema_connection).expect("fresh current schema");
    schema_connection
        .execute_batch("DROP TRIGGER validate_permission_retry_fence_before_update")
        .expect("drop V54 lifecycle trigger");
    let schema_error = run(&schema_connection).expect_err("stale V54 schema must fail closed");
    assert!(
        schema_error.to_string().contains("V54 marker"),
        "unexpected V54 schema audit error: {schema_error}"
    );

    let marker_connection = Connection::open_in_memory().expect("database");
    run(&marker_connection).expect("fresh current schema");
    marker_connection
        .execute(
            "UPDATE moyai_schema_migrations
                 SET name = 'tampered_v54'
                 WHERE version = ?1",
            [PERMISSION_RETRY_FENCES_VERSION],
        )
        .expect("tamper V54 marker");
    let marker_error = run(&marker_connection).expect_err("wrong V54 marker must fail closed");
    assert!(
        marker_error
            .to_string()
            .contains(PERMISSION_RETRY_FENCES_NAME),
        "unexpected V54 marker audit error: {marker_error}"
    );

    let data_connection = Connection::open_in_memory().expect("database");
    run(&data_connection).expect("fresh current schema");
    let session_id = crate::session::SessionId::new().to_string();
    let turn_id = crate::protocol::TurnId::new().to_string();
    let authority_id = crate::protocol::HistoryItemId::new().to_string();
    data_connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('v54-project', 'C:/v54', 'v54', 'none', 1, 1);",
        )
        .expect("V54 fixture project");
    data_connection
        .execute(
            "INSERT INTO sessions (
                     id, project_id, title, status, cwd_path, model_name, base_url,
                     created_at_ms, updated_at_ms, completed_at_ms,
                     active_run_id, active_turn_id, active_run_lease_expires_at_ms
                 )
                 VALUES (
                     ?1, 'v54-project', 'session', 'idle',
                     'C:/v54', 'model', 'http://localhost',
                     1, 1, NULL, NULL, NULL, NULL
                 )",
            [&session_id],
        )
        .expect("V54 fixture session");
    let authority_payload = serde_json::to_string(&crate::protocol::HistoryItemPayload::UserTurn {
        content: vec![crate::protocol::ContentPart::Text {
            text: "authority".to_string(),
        }],
        prompt_dispatch: None,
        editor_context: None,
    })
    .expect("V54 authority payload");
    insert_v51_history_fixture(
        &data_connection,
        &authority_id,
        &session_id,
        &turn_id,
        0,
        2,
        &authority_payload,
    );
    data_connection
        .execute(
            "INSERT INTO permission_retry_fences (
                     root_session_id, family_version, family_sha256,
                     authority_history_item_id, state, review_id, identity_sha256,
                     outcome, created_at_ms, updated_at_ms
                 ) VALUES (
                     ?1, 1, ?2, ?3, 'reviewing', 'not-a-ulid', ?4, NULL, 3, 3
                 )",
            params![
                session_id,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                authority_id,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ],
        )
        .expect("inject syntactically invalid V54 review id");
    let data_error = validate_permission_retry_fences_data(&data_connection)
        .expect_err("invalid V54 review identity must fail closed");
    assert!(
        data_error.to_string().contains("invalid review id"),
        "unexpected V54 data audit error: {data_error}"
    );
    assert!(
        run(&data_connection).is_err(),
        "current-schema reopen must execute the V54 data audit"
    );

    data_connection
        .execute("DELETE FROM permission_retry_fences", [])
        .expect("remove invalid review-id fixture");
    data_connection
        .pragma_update(None, "ignore_check_constraints", "ON")
        .expect("temporarily bypass V54 family-version constraint");
    data_connection
        .execute(
            "INSERT INTO permission_retry_fences (
                     root_session_id, family_version, family_sha256,
                     authority_history_item_id, state, review_id, identity_sha256,
                     outcome, created_at_ms, updated_at_ms
                 ) VALUES (
                     ?1, 4294967296, ?2, ?3, 'reviewing', ?4, ?5, NULL, 4, 4
                 )",
            params![
                session_id,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                authority_id,
                crate::storage::PermissionReviewId::new().to_string(),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ],
        )
        .expect("inject out-of-range V54 family version");
    data_connection
        .pragma_update(None, "ignore_check_constraints", "OFF")
        .expect("restore V54 family-version constraint");
    let version_error = validate_permission_retry_fences_data(&data_connection)
        .expect_err("family versions outside u32 must fail closed");
    assert!(
        version_error
            .to_string()
            .contains("permission retry fence row"),
        "unexpected V54 family-version audit error: {version_error}"
    );
    assert!(
        run(&data_connection).is_err(),
        "current-schema reopen must reject an out-of-range V54 family version"
    );
}

#[test]
fn v53_upgrades_exact_v52_reopens_idempotently_and_owns_exact_schema() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v51_endpoint(&connection);
    connection
        .execute_batch(V52_HARNESS_TURN_IDENTITY)
        .expect("exact V52 endpoint");

    run(&connection).expect("V52 to V53 upgrade");
    run(&connection).expect("idempotent V53 reopen");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            AGENT_TRIGGER_TURN_CLAIMS_VERSION,
            AGENT_TRIGGER_TURN_CLAIMS_NAME,
        )
        .expect("V53 marker")
    );
    validate_agent_trigger_turn_claims_schema(&connection).expect("exact V53 schema");
    validate_agent_trigger_turn_claims_data(&connection).expect("valid V53 data");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v53_rejects_invalid_v52_before_creating_schema_or_marker() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v51_endpoint(&connection);
    connection
        .execute_batch(V52_HARNESS_TURN_IDENTITY)
        .expect("exact V52 endpoint");
    connection
        .execute_batch("DROP INDEX idx_protocol_runtime_events_unique_turn_terminal")
        .expect("inject pre-existing V52 canonical-schema corruption");

    let error = run(&connection).expect_err("invalid V52 must fail before V53 mutation");
    assert!(
        error
            .to_string()
            .contains("idx_protocol_runtime_events_unique_turn_terminal"),
        "unexpected V52 prevalidation error: {error}"
    );
    assert!(
        !schema_migration_applied(&connection, AGENT_TRIGGER_TURN_CLAIMS_VERSION)
            .expect("V53 marker query"),
        "failed upgrade must leave the exact V52 marker endpoint"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM sqlite_master
                     WHERE name IN (
                         'agent_trigger_turn_claims',
                         'validate_agent_trigger_turn_claim_before_insert',
                         'prevent_agent_trigger_turn_claim_update',
                         'validate_claimed_agent_mailbox_resolution_before_update'
                     )",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("V53 schema object count"),
        0,
        "failed V52 prevalidation must not leave partial V53 schema"
    );
}

#[test]
fn v53_fast_path_rejects_schema_marker_and_claim_identity_tampering() {
    let schema_connection = Connection::open_in_memory().expect("database");
    run(&schema_connection).expect("fresh current schema");
    schema_connection
        .execute_batch("DROP TRIGGER prevent_agent_trigger_turn_claim_update")
        .expect("drop V53 immutability trigger");
    let schema_error = run(&schema_connection).expect_err("stale V53 schema must fail closed");
    assert!(
        schema_error.to_string().contains("V53 marker"),
        "unexpected V53 schema audit error: {schema_error}"
    );

    let mailbox_schema_connection = Connection::open_in_memory().expect("database");
    run(&mailbox_schema_connection).expect("fresh current schema");
    mailbox_schema_connection
        .execute_batch("DROP TRIGGER validate_claimed_agent_mailbox_resolution_before_update")
        .expect("drop V53 claimed-mailbox trigger");
    let mailbox_schema_error =
        run(&mailbox_schema_connection).expect_err("missing V53 mailbox trigger must fail");
    assert!(
        mailbox_schema_error
            .to_string()
            .contains("claimed-mailbox resolution trigger"),
        "unexpected V53 mailbox schema error: {mailbox_schema_error}"
    );

    let marker_connection = Connection::open_in_memory().expect("database");
    run(&marker_connection).expect("fresh current schema");
    marker_connection
        .execute(
            "UPDATE moyai_schema_migrations
                 SET name = 'tampered_v53'
                 WHERE version = ?1",
            [AGENT_TRIGGER_TURN_CLAIMS_VERSION],
        )
        .expect("tamper V53 marker");
    let marker_error = run(&marker_connection).expect_err("wrong V53 marker must fail closed");
    assert!(
        marker_error
            .to_string()
            .contains(AGENT_TRIGGER_TURN_CLAIMS_NAME)
    );

    let data_connection = Connection::open_in_memory().expect("database");
    run(&data_connection).expect("fresh current schema");
    data_connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for corruption fixture");
    data_connection
        .execute(
            "INSERT INTO agent_trigger_turn_claims
                 (history_item_id, recipient_session_id, admission_id, turn_id, created_at_ms)
                 VALUES ('missing-wake', 'missing-session', 'missing-admission',
                         'missing-turn', 1)",
            [],
        )
        .expect_err("canonical insert trigger rejects an orphan claim");
    let validation_trigger_sql = data_connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'validate_agent_trigger_turn_claim_before_insert'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("V53 validation trigger");
    data_connection
        .execute_batch("DROP TRIGGER validate_agent_trigger_turn_claim_before_insert")
        .expect("temporarily bypass V53 identity trigger");
    data_connection
        .execute(
            "INSERT INTO agent_trigger_turn_claims
                 (history_item_id, recipient_session_id, admission_id, turn_id, created_at_ms)
                 VALUES ('missing-wake', 'missing-session', 'missing-admission',
                         'missing-turn', 1)",
            [],
        )
        .expect("inject orphan V53 claim");
    data_connection
        .execute_batch(&validation_trigger_sql)
        .expect("restore V53 validation trigger");
    data_connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("restore foreign keys");
    let data_error = validate_agent_trigger_turn_claims_data(&data_connection)
        .expect_err("orphan V53 claim must fail closed");
    assert!(
        data_error.to_string().contains("explicit agent wake claim"),
        "unexpected V53 claim audit error: {data_error}"
    );
    assert!(
        run(&data_connection).is_err(),
        "current-schema reopen must execute the V53 claim audit"
    );
}

#[test]
fn v53_claimed_mailbox_discard_requires_its_interrupted_terminal() {
    let connection =
        canonical_agent_trigger_turn_claims_connection().expect("canonical V53 database");
    connection
        .execute(
            "INSERT INTO sessions (
                     id, status, active_run_id, active_turn_id,
                     active_run_lease_expires_at_ms
                 )
                 VALUES ('session-1', 'running', 'admission-1', 'turn-1', 100)",
            [],
        )
        .expect("active session owner");
    connection
        .execute(
            "INSERT INTO agent_mailbox_messages (
                     id, recipient_session_id, trigger_turn, state
                 )
                 VALUES ('wake-1', 'session-1', 1, 'pending')",
            [],
        )
        .expect("pending explicit wake");
    connection
        .execute(
            "INSERT INTO agent_trigger_turn_claims (
                     history_item_id, recipient_session_id, admission_id,
                     turn_id, created_at_ms
                 )
                 VALUES ('wake-1', 'session-1', 'admission-1', 'turn-1', 1)",
            [],
        )
        .expect("exact wake claim");
    connection
        .execute(
            "INSERT INTO protocol_runtime_events (
                     id, session_id, turn_id, msg_json
                 )
                 VALUES (
                     'failed-terminal', 'session-1', 'turn-1',
                     '{\"kind\":\"turn_terminal\",\"terminal\":{\"outcome\":{\"kind\":\"failed\"}}}'
                 )",
            [],
        )
        .expect("failed terminal");

    let failed_discard = connection
        .execute(
            "UPDATE agent_mailbox_messages
                 SET state = 'discarded',
                     resolved_by_terminal_event_id = 'failed-terminal'
                 WHERE id = 'wake-1'",
            [],
        )
        .expect_err("Failed terminal must not discard its claimed wake");
    assert!(
        failed_discard
            .to_string()
            .contains("claimed agent mailbox resolution"),
        "unexpected failed-discard error: {failed_discard}"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM agent_mailbox_messages WHERE id = 'wake-1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("wake state after rejected discard"),
        "pending"
    );

    connection
            .execute(
                "INSERT INTO protocol_runtime_events (
                     id, session_id, turn_id, msg_json
                 )
                 VALUES (
                     'interrupted-terminal', 'session-1', 'turn-1',
                     '{\"kind\":\"turn_terminal\",\"terminal\":{\"outcome\":{\"kind\":\"interrupted\"}}}'
                 )",
                [],
            )
            .expect("interrupted terminal");
    assert_eq!(
        connection
            .execute(
                "UPDATE agent_mailbox_messages
                     SET state = 'discarded',
                         resolved_by_terminal_event_id = 'interrupted-terminal'
                     WHERE id = 'wake-1'",
                [],
            )
            .expect("Interrupted terminal owns exact wake discard"),
        1
    );
    validate_agent_trigger_turn_claims_data(&connection)
        .expect("interrupted discard remains valid V53 data");
}

#[test]
fn v51_rejects_invalid_delivery_order_and_non_interrupted_discard_resolution() {
    let (order_connection, _, _, _, order_input_id, _) =
        current_v51_with_one_queued_runtime_steer();
    let immutable_order_trigger = order_connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'prevent_turn_steer_enqueue_order_update'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("V51 immutable order trigger");
    order_connection
        .execute_batch("DROP TRIGGER prevent_turn_steer_enqueue_order_update")
        .expect("temporarily bypass immutable queue order");
    order_connection
        .execute(
            "UPDATE turn_steer_input_enqueue_order
                 SET created_at_ms = 101
                 WHERE input_id = ?1",
            [&order_input_id],
        )
        .expect("tamper enqueue timestamp");
    order_connection
        .execute_batch(&immutable_order_trigger)
        .expect("restore immutable queue-order trigger");
    let order_error = validate_durable_turn_input_queue_data(&order_connection)
        .expect_err("mismatched enqueue identity must fail closed");
    assert!(
        order_error.to_string().contains("turn-steer input"),
        "unexpected V51 order audit error: {order_error}"
    );
    assert!(
        run(&order_connection).is_err(),
        "the current-schema reopen must run the V51 data audit"
    );

    let (
        delivered_connection,
        delivered_session_id,
        _,
        delivered_turn_id,
        delivered_input_id,
        delivered_payload_json,
    ) = current_v51_with_one_queued_runtime_steer();
    delivered_connection
        .execute(
            "INSERT INTO protocol_history_items (
                     id, session_id, scope_kind, turn_id, sequence_no,
                     payload_json, payload_sha256, created_at_ms
                 )
                 VALUES (?1, ?2, 'turn', ?3, 0, ?4, ?5, 110)",
            params![
                delivered_input_id,
                delivered_session_id,
                delivered_turn_id,
                delivered_payload_json,
                sha256_text(&delivered_payload_json),
            ],
        )
        .expect("delivered history without append-order provenance");
    let delivered_transition_trigger = delivered_connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'validate_turn_steer_input_transition_before_update'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("V51 delivery transition trigger");
    delivered_connection
        .execute_batch("DROP TRIGGER validate_turn_steer_input_transition_before_update")
        .expect("temporarily bypass delivery transition");
    delivered_connection
        .execute(
            "UPDATE turn_steer_inputs
                 SET state = 'delivered',
                     delivered_history_item_id = id,
                     delivered_at_ms = 110,
                     updated_at_ms = 110
                 WHERE id = ?1",
            [&delivered_input_id],
        )
        .expect("inject delivered row without append order");
    delivered_connection
        .execute_batch(&delivered_transition_trigger)
        .expect("restore delivery transition trigger");
    let delivered_error = validate_durable_turn_input_queue_data(&delivered_connection)
        .expect_err("delivered input without exact append order must fail closed");
    assert!(
        delivered_error.to_string().contains("turn-steer input"),
        "unexpected V51 delivery audit error: {delivered_error}"
    );

    let (discard_connection, discard_session_id, _, discard_turn_id, discard_input_id, _) =
        current_v51_with_one_queued_runtime_steer();
    let terminal_id = crate::protocol::RuntimeEventId::new().to_string();
    let terminal_json = serde_json::to_string(&crate::protocol::RuntimeEventMsg::TurnTerminal {
        terminal: Box::new(crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    })
    .expect("typed completed terminal");
    discard_connection
        .execute(
            "INSERT INTO protocol_runtime_events (
                     id, session_id, turn_id, sequence_no, msg_json,
                     payload_sha256, created_at_ms
                 )
                 VALUES (?1, ?2, ?3, 0, ?4, ?5, 110)",
            params![
                terminal_id,
                discard_session_id,
                discard_turn_id,
                terminal_json,
                sha256_text(&terminal_json),
            ],
        )
        .expect("completed terminal fixture");
    discard_connection
        .execute(
            "INSERT INTO protocol_item_append_order (
                     session_id, scope_kind, turn_id, sequence_no,
                     source_kind, source_id, created_at_ms
                 )
                 VALUES (?1, 'turn', ?2, 0, 'runtime_event', ?3, 110)",
            params![discard_session_id, discard_turn_id, terminal_id],
        )
        .expect("completed terminal append order");
    let direct_discard_error = discard_connection
        .execute(
            "UPDATE turn_steer_inputs
                 SET state = 'discarded',
                     resolved_by_terminal_event_id = ?2,
                     discarded_at_ms = 110,
                     updated_at_ms = 110
                 WHERE id = ?1",
            params![discard_input_id, terminal_id],
        )
        .expect_err("completed terminal must not resolve a discarded steer");
    assert!(
        direct_discard_error
            .to_string()
            .contains("lifecycle transition"),
        "unexpected V51 discard transition error: {direct_discard_error}"
    );
    let discard_transition_trigger = discard_connection
        .query_row(
            "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name = 'validate_turn_steer_input_transition_before_update'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("V51 discard transition trigger");
    discard_connection
        .execute_batch("DROP TRIGGER validate_turn_steer_input_transition_before_update")
        .expect("temporarily bypass discard transition");
    discard_connection
        .execute(
            "UPDATE turn_steer_inputs
                 SET state = 'discarded',
                     resolved_by_terminal_event_id = ?2,
                     discarded_at_ms = 110,
                     updated_at_ms = 110
                 WHERE id = ?1",
            params![discard_input_id, terminal_id],
        )
        .expect("inject invalid discard resolver");
    discard_connection
        .execute_batch(&discard_transition_trigger)
        .expect("restore discard transition trigger");
    let discard_error = validate_durable_turn_input_queue_data(&discard_connection)
        .expect_err("non-interrupted discard resolver must fail closed");
    assert!(
        discard_error.to_string().contains("turn-steer input"),
        "unexpected V51 discard audit error: {discard_error}"
    );
}

#[test]
fn v50_upgrades_exact_v49_endpoint_and_is_idempotent() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v49_endpoint(&connection);

    run(&connection).expect("V49 to V50 upgrade");
    run(&connection).expect("idempotent V50 validation");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            DURABLE_AGENT_MAILBOX_VERSION,
            DURABLE_AGENT_MAILBOX_NAME,
        )
        .expect("V50 marker")
    );
    validate_durable_agent_mailbox_schema(&connection).expect("exact V50 schema");
    validate_durable_agent_mailbox_data(&connection).expect("valid V50 data");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v50_backfills_only_canonical_live_mail_and_preserves_append_boundary() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v49_endpoint(&connection);
    connection
        .execute_batch(
            "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES ('v50-project', 'C:/v50', 'v50', 'none', 1, 1);
                 INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES
                 ('v50-root', 'v50-project', 'root', 'idle', 'C:/v50',
                  'model', 'http://localhost', 1, 1, NULL),
                 ('v50-child', 'v50-project', 'child', 'idle', 'C:/v50',
                  'model', 'http://localhost', 2, 2, NULL);
                 INSERT INTO session_spawn_edges
                 (root_session_id, parent_session_id, child_session_id,
                  agent_path, task_name, spawn_order, created_at_ms)
                 VALUES ('v50-root', 'v50-root', 'v50-child',
                         '/root/child', 'child', 1, 2);",
        )
        .expect("V50 agent tree");
    let pending = serde_json::json!({
            "kind": "inter_agent_communication",
            "communication": {
                "author": "/root/child",
                "recipient": "/root",
                "content": "Message Type: MESSAGE\nTask name: /root\nSender: /root/child\nPayload:\nqueued",
                "trigger_turn": true
            }
        })
        .to_string();
    let delivered = serde_json::json!({
            "kind": "inter_agent_communication",
            "communication": {
                "author": "/root/child",
                "recipient": "/root",
                "content": "Message Type: MESSAGE\nTask name: /root\nSender: /root/child\nPayload:\ndelivered",
                "trigger_turn": false
            }
        })
        .to_string();
    let inert_copy = serde_json::json!({
        "kind": "inter_agent_communication",
        "communication": {
            "author": "/root",
            "recipient": "/root/child",
            "content": "fork context copy",
            "trigger_turn": false
        }
    })
    .to_string();
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no,
                  payload_json, payload_sha256, created_at_ms)
                 VALUES ('v50-delivered', 'v50-root', 'turn', 'root-turn',
                         0, ?1, ?2, 4)",
            params![delivered, sha256_text(&delivered)],
        )
        .expect("delivered history");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES ('v50-root', 'turn', 'root-turn', 0,
                         'history_item', 'v50-delivered', 4)",
            [],
        )
        .expect("delivered append order");
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no,
                  payload_json, payload_sha256, created_at_ms)
                 VALUES ('v50-pending', 'v50-root', 'session', NULL, 0, ?1, ?2, 5)",
            params![pending, sha256_text(&pending)],
        )
        .expect("pending history");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES ('v50-root', 'session', NULL, 0,
                         'history_item', 'v50-pending', 5)",
            [],
        )
        .expect("pending append order");
    let pending_position = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO protocol_history_items
                 (id, session_id, scope_kind, turn_id, sequence_no,
                  payload_json, payload_sha256, created_at_ms)
                 VALUES ('v50-inert-copy', 'v50-root', 'session', NULL, 1,
                         ?1, ?2, 6)",
            params![inert_copy, sha256_text(&inert_copy)],
        )
        .expect("inert fork history");
    connection
        .execute(
            "INSERT INTO protocol_item_append_order
                 (session_id, scope_kind, turn_id, sequence_no,
                  source_kind, source_id, created_at_ms)
                 VALUES ('v50-root', 'session', NULL, 1,
                         'history_item', 'v50-inert-copy', 6)",
            [],
        )
        .expect("inert fork append order");

    run(&connection).expect("V50 classified backfill");

    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM agent_mailbox_messages
                     WHERE id = 'v50-pending'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("pending mailbox"),
        "pending"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT source_kind || ':' || append_position
                     FROM protocol_item_append_order
                     WHERE source_id = 'v50-pending'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("pending enqueue order"),
        format!("mailbox_message:{pending_position}")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_history_items
                     WHERE id = 'v50-pending'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("pending history removed"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM agent_mailbox_messages
                     WHERE id = 'v50-delivered'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("delivered mailbox"),
        "delivered"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM agent_mailbox_messages
                     WHERE id = 'v50-inert-copy'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("inert mailbox count"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_history_items
                     WHERE id = 'v50-inert-copy'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("inert history retained"),
        1
    );
    validate_durable_agent_mailbox_data(&connection).expect("backfilled V50 data");

    let pending_delete_error = connection
        .execute("DELETE FROM sessions WHERE id = 'v50-child'", [])
        .expect_err("pending outgoing mailbox must block author deletion");
    assert!(
        pending_delete_error
            .to_string()
            .contains("invalid agent mailbox lifecycle transition"),
        "unexpected pending-author deletion error: {pending_delete_error}"
    );
    connection
        .execute_batch(
            "DELETE FROM protocol_item_append_order
                 WHERE source_kind = 'mailbox_message'
                   AND source_id = 'v50-pending';
                 DELETE FROM agent_mailbox_messages
                 WHERE id = 'v50-pending';
                 DELETE FROM sessions WHERE id = 'v50-child';",
        )
        .expect("delete author after pending mail is explicitly removed");
    assert_eq!(
        connection
            .query_row(
                "SELECT author_session_id IS NULL
                     FROM agent_mailbox_messages
                     WHERE id = 'v50-delivered'",
                [],
                |row| row.get::<_, bool>(0),
            )
            .expect("delivered mailbox author tombstone"),
        true
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM protocol_history_items
                     WHERE id = 'v50-delivered'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("ancestor delivered history retained"),
        1
    );
    validate_durable_agent_mailbox_data(&connection)
        .expect("author tombstone preserves delivered owner");
}

#[test]
fn v50_fast_path_rejects_schema_tampering() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run(&connection).expect("fresh V50 schema");
    connection
        .execute_batch("DROP TRIGGER validate_agent_mailbox_message_before_update;")
        .expect("tamper lifecycle trigger");

    let error = run(&connection).expect_err("V50 exact schema must reject tampering");
    assert!(
        error
            .to_string()
            .contains("validate_agent_mailbox_message_before_update"),
        "unexpected V50 schema error: {error}"
    );
}

#[test]
fn v59_upgrades_v58_with_zero_revision_and_preserves_context_window_json() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v57_endpoint(&connection);
    run_exact_execution_interrupt_requests(&connection).expect("V58 endpoint");
    let session_id = insert_v56_session(&connection);
    connection
            .execute(
                "UPDATE sessions
                 SET model_parameters_json = '{\"context_window\":131072,\"max_output_tokens\":8192}'
                 WHERE id = ?1",
                [&session_id],
            )
            .expect("V58 context-window-compatible JSON");

    run_to_current(&connection).expect("V58 to V59 current-endpoint upgrade");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION,
            SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_NAME,
        )
        .expect("V59 marker")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT session_settings_revision,
                            json_extract(model_parameters_json, '$.context_window')
                     FROM sessions WHERE id = ?1",
                [&session_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("migrated session settings"),
        (0, 131_072)
    );
    run(&connection).expect("V59 idempotent reopen");
}

#[test]
fn v59_invalid_context_window_rolls_back_column_and_marker_atomically() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v57_endpoint(&connection);
    run_exact_execution_interrupt_requests(&connection).expect("V58 endpoint");
    let session_id = insert_v56_session(&connection);
    connection
        .execute(
            "UPDATE sessions
                 SET model_parameters_json = '{\"context_window\":0}'
                 WHERE id = ?1",
            [&session_id],
        )
        .expect("invalid legacy context window fixture");

    let error = run_to_current(&connection)
        .expect_err("invalid context window must roll back the forward migration");
    assert!(
        error
            .to_string()
            .contains("invalid revision or context window")
    );
    assert!(
        !schema_migration_applied(
            &connection,
            SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION,
        )
        .expect("V59 marker absence")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions')
                     WHERE name = 'session_settings_revision'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("rolled-back column count"),
        0
    );

    connection
        .execute(
            "UPDATE sessions SET model_parameters_json = '{}' WHERE id = ?1",
            [&session_id],
        )
        .expect("repair legacy context window");
    run_to_current(&connection).expect("repaired V59 migration");
}

#[test]
fn v59_fast_path_rejects_a_regressed_settings_revision() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_to_current(&connection).expect("fresh current schema");
    let session_id = insert_v56_session(&connection);
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .expect("enable corruption fixture");
    connection
        .execute(
            "UPDATE sessions SET session_settings_revision = -1 WHERE id = ?1",
            [&session_id],
        )
        .expect("regress settings revision");
    connection
        .execute_batch("PRAGMA ignore_check_constraints = OFF")
        .expect("restore constraints");

    let error = run(&connection).expect_err("regressed revision must fail closed");
    assert!(
        error
            .to_string()
            .contains("invalid revision or context window")
    );
}

#[test]
fn v59_fast_path_rejects_a_revision_column_without_its_canonical_check() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v57_endpoint(&connection);
    run_exact_execution_interrupt_requests(&connection).expect("V58 endpoint");
    let session_id = insert_v56_session(&connection);
    connection
        .execute_batch(
            "ALTER TABLE sessions
                 ADD COLUMN session_settings_revision INTEGER NOT NULL DEFAULT 0;",
        )
        .expect("non-canonical V59 column fixture");
    connection
        .execute(
            "INSERT INTO moyai_schema_migrations(version, name) VALUES (?1, ?2)",
            (
                SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION,
                SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_NAME,
            ),
        )
        .expect("forged V59 marker");
    assert_eq!(
        connection
            .query_row(
                "SELECT session_settings_revision FROM sessions WHERE id = ?1",
                [&session_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("valid forged V59 row"),
        0
    );

    let error = run(&connection).expect_err("missing V59 CHECK must fail closed on reopen");
    assert!(
        error.to_string().contains("stale CHECK constraints"),
        "unexpected V59 schema error: {error}"
    );
}

#[test]
fn v60_maps_all_legacy_side_chat_provider_pairs_and_preserves_unbound_roots() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v59_endpoint(&connection);
    let cases = [
        ("lm_studio_native_required", "responses", "lm_studio"),
        (
            "openai_compatible_only",
            "chat_completions",
            "openai_compatible",
        ),
        ("openai_compatible_only", "responses", "openai_responses"),
        (
            "lm_studio_native_required",
            "chat_completions",
            "lm_studio_chat_completions",
        ),
    ];
    let mut fixtures = Vec::new();
    for (metadata_mode, api_mode, expected_profile) in cases {
        let (owner_id, conversation_id, binding_id) =
            insert_v59_side_chat(&connection, metadata_mode, api_mode);
        fixtures.push((owner_id, conversation_id, binding_id, expected_profile));
    }

    run_provider_connection_profiles(&connection).expect("V59 to V60 migration");
    validate_provider_connection_profiles_schema(&connection).expect("idempotent V60 schema audit");
    validate_provider_connection_profiles_data(&connection).expect("idempotent V60 data audit");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            PROVIDER_CONNECTION_PROFILES_VERSION,
            PROVIDER_CONNECTION_PROFILES_NAME,
        )
        .expect("V60 marker")
    );
    for (owner_id, conversation_id, binding_id, expected_profile) in fixtures {
        assert_eq!(
            connection
                .query_row(
                    "SELECT provider_profile FROM side_chat_bindings WHERE id = ?1",
                    [&binding_id],
                    |row| row.get::<_, String>(0),
                )
                .expect("migrated binding profile"),
            expected_profile
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT json_extract(provider_connection_json, '$.profile'),
                                json_type(provider_connection_json, '$.api_key_env'),
                                (SELECT COUNT(*)
                                 FROM json_each(provider_connection_json, '$.extra_headers'))
                         FROM sessions WHERE id = ?1",
                    [&conversation_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .expect("migrated hidden connection"),
            (expected_profile.to_string(), "null".to_string(), 0)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT provider_connection_json IS NULL FROM sessions WHERE id = ?1",
                    [&owner_id],
                    |row| row.get::<_, bool>(0),
                )
                .expect("legacy owner connection"),
            true,
            "ordinary pre-V60 roots must remain explicitly unbound"
        );
    }
    validate_provider_connection_profiles_schema(&connection).expect("canonical V60 schema");
    validate_provider_connection_profiles_data(&connection).expect("canonical V60 data");
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v60_reopen_rejects_malformed_unknown_and_invalid_provider_snapshots() {
    let corruptions = [
        "{",
        r#"{"profile":"openai_compatible","api_key_env":null,"extra_headers":{},"future":true}"#,
        r#"{"profile":"openai_compatible","api_key_env":"INVALID-NAME","extra_headers":{}}"#,
        r#"{"profile":"openai_compatible","api_key_env":null,"extra_headers":{"bad header":"value"}}"#,
    ];
    for corruption in corruptions {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_to_current(&connection).expect("fresh V60 schema");
        let session_id = insert_v56_session(&connection);
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .expect("enable corruption fixture");
        connection
            .execute(
                "UPDATE sessions SET provider_connection_json = ?2 WHERE id = ?1",
                params![session_id, corruption],
            )
            .expect("inject invalid provider snapshot");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF")
            .expect("restore constraints");

        let error = run(&connection).expect_err("invalid provider snapshot must fail closed");
        assert!(
            error.to_string().contains("provider connection"),
            "unexpected V60 data audit error: {error}"
        );
    }
}

#[test]
fn v60_write_boundary_rejects_non_text_custom_header_values() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_to_current(&connection).expect("fresh V60 schema");
    let session_id = insert_v56_session(&connection);

    let error = connection
            .execute(
                "UPDATE sessions SET provider_connection_json = ?2 WHERE id = ?1",
                params![
                    &session_id,
                    r#"{"profile":"openai_compatible","api_key_env":null,"extra_headers":{"Authorization":7}}"#
                ],
            )
            .expect_err("numeric custom-header value must fail with constraints enabled");

    assert!(
        error
            .to_string()
            .contains("custom-header values must be JSON strings")
    );
    assert!(
        connection
            .query_row(
                "SELECT provider_connection_json IS NULL FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get::<_, bool>(0),
            )
            .expect("preserved provider snapshot")
    );
}

#[test]
fn v60_reopen_rejects_provider_profile_trigger_tampering() {
    let connection = Connection::open_in_memory().expect("database");
    run_to_current(&connection).expect("fresh V60 schema");
    connection
        .execute_batch("DROP TRIGGER validate_side_chat_binding_before_update")
        .expect("tamper V60 provider transition trigger");

    let error = run(&connection).expect_err("stale V60 schema must fail closed");
    assert!(
        error.to_string().contains("canonical system-prompt schema"),
        "unexpected V60 schema audit error: {error}"
    );

    let session_trigger_connection = Connection::open_in_memory().expect("database");
    run_to_current(&session_trigger_connection).expect("fresh V60 schema");
    session_trigger_connection
        .execute_batch("DROP TRIGGER validate_session_provider_connection_headers_before_update")
        .expect("tamper V60 session provider trigger");

    let session_trigger_error = run(&session_trigger_connection)
        .expect_err("stale V60 session provider trigger must fail closed");
    assert!(
        session_trigger_error
            .to_string()
            .contains("provider-connection validation triggers"),
        "unexpected V60 session trigger schema error: {session_trigger_error}"
    );
}

#[test]
fn v60_invalid_legacy_provider_pair_rolls_back_atomically() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v59_endpoint(&connection);
    let (_, _, binding_id) =
        insert_v59_side_chat(&connection, "lm_studio_native_required", "responses");
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .expect("enable legacy corruption fixture");
    connection
        .execute(
            "UPDATE side_chat_bindings
                 SET provider_metadata_mode = 'unknown_catalog'
                 WHERE id = ?1",
            [&binding_id],
        )
        .expect("inject unmappable legacy provider pair");
    connection
        .execute_batch("PRAGMA ignore_check_constraints = OFF")
        .expect("restore constraints");

    run(&connection).expect_err("unmappable legacy pair must roll back V60");
    assert!(
        !schema_migration_applied(&connection, PROVIDER_CONNECTION_PROFILES_VERSION)
            .expect("V60 marker absence")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions')
                     WHERE name = 'provider_connection_json'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("rolled-back connection column"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('side_chat_bindings')
                     WHERE name = 'provider_metadata_mode'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("legacy side-chat column retained"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('side_chat_bindings')
                     WHERE name = 'provider_profile'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("new side-chat column absent"),
        0
    );
}

#[test]
fn v61_fresh_schema_records_exact_marker_and_owns_prompt_constraints() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_to_current(&connection).expect("fresh V61 schema");

    assert!(
        schema_migration_has_exact_name(
            &connection,
            SIDE_CHAT_SYSTEM_PROMPT_VERSION,
            SIDE_CHAT_SYSTEM_PROMPT_NAME,
        )
        .expect("V61 marker")
    );
    validate_side_chat_system_prompt_schema(&connection).expect("canonical V61 schema");
    validate_side_chat_system_prompt_data(&connection).expect("canonical V61 data");
    assert_eq!(
        connection
            .query_row(
                "SELECT type, \"notnull\", dflt_value
                     FROM pragma_table_info('side_chat_bindings')
                     WHERE name = 'system_prompt'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("system prompt column"),
        ("TEXT".to_string(), 1, "''".to_string())
    );
    assert!(foreign_key_violations(&connection).is_empty());
}

#[test]
fn v61_migrates_v60_rows_to_empty_prompt_and_reopens_idempotently() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v59_endpoint(&connection);
    let (_, _, binding_id) =
        insert_v59_side_chat(&connection, "openai_compatible_only", "chat_completions");
    run_provider_connection_profiles(&connection).expect("V60 endpoint");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('side_chat_bindings')
                     WHERE name = 'system_prompt'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("V60 column absence"),
        0
    );

    run(&connection).expect("V60 to V61 migration");
    run(&connection).expect("idempotent V61 reopen");
    assert_eq!(
        connection
            .query_row(
                "SELECT system_prompt FROM side_chat_bindings WHERE id = ?1",
                [&binding_id],
                |row| row.get::<_, String>(0),
            )
            .expect("migrated prompt"),
        ""
    );
    validate_side_chat_system_prompt_schema(&connection).expect("canonical V61 schema");
    validate_side_chat_system_prompt_data(&connection).expect("canonical V61 data");
}

#[test]
fn v61_write_boundary_enforces_unicode_character_limit_and_canonical_whitespace() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_through_exact_v59_endpoint(&connection);
    let (_, _, binding_id) =
        insert_v59_side_chat(&connection, "openai_compatible_only", "chat_completions");
    run_provider_connection_profiles(&connection).expect("V60 endpoint");
    run_side_chat_system_prompt(&connection).expect("V61 endpoint");

    let accepted = "界".repeat(crate::system_prompt::MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS);
    connection
        .execute(
            "UPDATE side_chat_bindings
                 SET system_prompt = ?2, updated_at_ms = updated_at_ms + 1
                 WHERE id = ?1",
            params![binding_id, accepted],
        )
        .expect("Unicode boundary prompt");
    let rejected = "界".repeat(crate::system_prompt::MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS + 1);
    connection
        .execute(
            "UPDATE side_chat_bindings
                 SET system_prompt = ?2, updated_at_ms = updated_at_ms + 1
                 WHERE id = ?1",
            params![binding_id, rejected],
        )
        .expect_err("oversized Unicode prompt must fail at the write boundary");
    for rejected in [
        "\tnoncanonical",
        "noncanonical\n",
        "\u{85}noncanonical",
        "\u{a0}noncanonical",
        "\u{1680}noncanonical",
        "\u{2003}noncanonical",
        "\u{2028}noncanonical",
        "\u{202f}noncanonical",
        "\u{205f}noncanonical",
        "\u{3000}noncanonical",
    ] {
        connection
            .execute(
                "UPDATE side_chat_bindings
                     SET system_prompt = ?2, updated_at_ms = updated_at_ms + 1
                     WHERE id = ?1",
                params![binding_id, rejected],
            )
            .expect_err("non-canonical Unicode whitespace must fail at the write boundary");
    }
    run(&connection).expect("valid boundary prompt must reopen");
}

#[test]
fn v61_reopen_rejects_prompt_data_corruption() {
    let corruptions = [
        (
            "界".repeat(crate::system_prompt::MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS + 1),
            "character-limit contract",
        ),
        (
            "\u{2003}noncanonical boundary".to_string(),
            "non-canonical system prompt boundary",
        ),
    ];
    for (corruption, expected_error) in corruptions {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        run_through_exact_v59_endpoint(&connection);
        let (_, _, binding_id) =
            insert_v59_side_chat(&connection, "openai_compatible_only", "chat_completions");
        run_provider_connection_profiles(&connection).expect("V60 endpoint");
        run_side_chat_system_prompt(&connection).expect("V61 endpoint");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .expect("enable corruption fixture");
        connection
            .execute(
                "UPDATE side_chat_bindings
                     SET system_prompt = ?2, updated_at_ms = updated_at_ms + 1
                     WHERE id = ?1",
                params![binding_id, corruption],
            )
            .expect("inject prompt corruption");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF")
            .expect("restore constraints");

        let error = run(&connection).expect_err("prompt corruption must fail closed");
        assert!(
            error.to_string().contains(expected_error),
            "unexpected V61 data audit error: {error}"
        );
    }
}

fn v69_predecessor_fixture(endpoint: i64) -> (Connection, String, String, String) {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    run_through_exact_v59_endpoint(&connection);
    let (owner, conversation, binding) =
        insert_v59_side_chat(&connection, "openai_compatible_only", "chat_completions");
    run_provider_connection_profiles(&connection).unwrap();
    if endpoint >= 61 {
        run_side_chat_system_prompt(&connection).unwrap();
    }
    if endpoint >= 65 {
        run_remote_agent_jobs(&connection).unwrap();
        run_device_outgoing_references(&connection).unwrap();
        run_remote_network_receipts(&connection).unwrap();
        run_remote_artifacts(&connection).unwrap();
    }
    if endpoint >= 66 {
        run_side_chat_route_kind(&connection).unwrap();
    }
    if endpoint >= 67 {
        run_shared_run_checkpoints(&connection).unwrap();
    }
    if endpoint >= 68 {
        run_shared_history_files(&connection).unwrap();
    }
    connection.execute(
            "UPDATE side_chat_bindings SET persisted_draft='retained draft',draft_revision=1,updated_at_ms=3 WHERE id=?1",
            [&binding],
        ).unwrap();
    (connection, owner, conversation, binding)
}

fn v69_side_snapshot(connection: &Connection, binding: &str) -> String {
    connection.query_row(
            "SELECT json_object('id',b.id,'owner',b.owner_session_id,'conversation',b.conversation_session_id,
                'url',b.base_url,'model',b.model,'profile',b.provider_profile,
                'draft',b.persisted_draft,'draft_revision',b.draft_revision,'generation',b.request_generation,
                'connection',s.provider_connection_json)
             FROM side_chat_bindings b JOIN sessions s ON s.id=b.conversation_session_id WHERE b.id=?1",
            [binding], |row| row.get(0),
        ).unwrap()
}

#[test]
fn v69_upgrades_released_side_snapshots_without_rebinding() {
    for endpoint in [60, 61, 65, 66, 67, 68] {
        let (connection, owner, conversation, binding) = v69_predecessor_fixture(endpoint);
        let before = v69_side_snapshot(&connection, &binding);
        run_to_current(&connection).expect("released storage reaches V69");
        run_to_current(&connection).expect("V69 cold audit is idempotent");
        assert!(
            schema_migration_has_exact_name(
                &connection,
                SIDE_CHAT_API_KEY_REFERENCES_VERSION,
                SIDE_CHAT_API_KEY_REFERENCES_NAME,
            )
            .unwrap()
        );
        assert_eq!(
            v69_side_snapshot(&connection, &binding),
            before,
            "V{endpoint}"
        );
        let reference: Option<String> = connection.query_row(
                "SELECT json_extract(provider_connection_json,'$.api_key_env') FROM sessions WHERE id=?1",
                [&conversation], |row| row.get(0),
            ).unwrap();
        assert_eq!(reference, None);
        let unbound: bool = connection
            .query_row(
                "SELECT provider_connection_json IS NULL FROM sessions WHERE id=?1",
                [&owner],
                |row| row.get(0),
            )
            .unwrap();
        assert!(unbound, "legacy Main root stays explicitly unbound");
        assert!(foreign_key_violations(&connection).is_empty());

        connection.execute(
                "UPDATE sessions SET provider_connection_json=json_set(provider_connection_json,'$.api_key_env','SIDE_KEY') WHERE id=?1",
                [&conversation],
            ).unwrap();
        connection.execute(
                "UPDATE side_chat_bindings SET persisted_draft='authenticated draft',draft_revision=2,updated_at_ms=4 WHERE id=?1",
                [&binding],
            ).expect("explicit reference permits canonical draft transition");
        run_to_current(&connection).expect("explicit reference reopens after upgrade");
    }
}

#[test]
fn v69_fresh_schema_rejects_marker_and_ownership_trigger_tampering() {
    for corruption in ["marker", "insert", "update"] {
        let connection = Connection::open_in_memory().unwrap();
        run_to_current(&connection).expect("fresh V69 schema");
        assert!(
            schema_migration_has_exact_name(
                &connection,
                SIDE_CHAT_API_KEY_REFERENCES_VERSION,
                SIDE_CHAT_API_KEY_REFERENCES_NAME,
            )
            .unwrap()
        );
        match corruption {
            "marker" => {
                connection
                    .execute(
                        "UPDATE moyai_schema_migrations SET name='incorrect' WHERE version=69",
                        [],
                    )
                    .unwrap();
            }
            "insert" => {
                connection
                    .execute_batch("DROP TRIGGER validate_side_chat_binding_before_insert")
                    .unwrap();
            }
            "update" => {
                connection
                    .execute_batch("DROP TRIGGER validate_side_chat_binding_before_update")
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let error = run_to_current(&connection)
            .expect_err("altered V69 ownership contract must fail closed");
        assert!(
            error.to_string().contains("migration marker")
                || error.to_string().contains("canonical system-prompt schema"),
            "{corruption}: {error}"
        );
    }
}

#[test]
fn v70_checkpoint_marker_is_forward_only_and_idempotent() {
    let connection = Connection::open_in_memory().unwrap();
    run_to_current(&connection).unwrap();
    connection
        .execute("DELETE FROM moyai_schema_migrations WHERE version=70", [])
        .unwrap();
    insert_v46_compaction_parent(&connection);
    let replacement_id = crate::protocol::HistoryItemId::new().to_string();
    insert_v46_replacement(&connection, &replacement_id, 0);
    let legacy = serde_json::json!({"kind":"compaction", "mode":"automatic",
            "layout":"legacy_prefix", "preserved_user_messages":[],
            "summary":"old summary", "replacement_item_ids":[replacement_id]})
    .to_string();
    insert_v46_compaction(
        &connection,
        "v70-old-checkpoint",
        1,
        &legacy,
        &sha256_text(&legacy),
    );
    let schema_before = text_snapshot(
        &connection,
        "SELECT json_group_array(json_array(type,name,sql)) FROM (SELECT type,name,sql FROM sqlite_master ORDER BY type,name)",
    );
    let payload_before = text_snapshot(
        &connection,
        "SELECT json_group_array(json_array(id,payload_json,payload_sha256)) FROM (SELECT * FROM protocol_history_items ORDER BY id)",
    );
    run_to_current(&connection).unwrap();
    run_to_current(&connection).unwrap();
    assert!(schema_migration_has_exact_name(&connection, 70, "clm_checkpoint").unwrap());
    assert_eq!(
        schema_before,
        text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(type,name,sql)) FROM (SELECT type,name,sql FROM sqlite_master ORDER BY type,name)"
        )
    );
    assert_eq!(
        payload_before,
        text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(id,payload_json,payload_sha256)) FROM (SELECT * FROM protocol_history_items ORDER BY id)"
        )
    );
    connection
        .execute(
            "UPDATE moyai_schema_migrations SET name='wrong' WHERE version=70",
            [],
        )
        .unwrap();
    assert!(
        run_to_current(&connection)
            .unwrap_err()
            .to_string()
            .contains("CLM checkpoint migration marker")
    );
}

#[test]
fn v70_checkpoint_failed_marker_write_is_retryable_without_rewriting_data() {
    let connection = Connection::open_in_memory().unwrap();
    run_to_current(&connection).unwrap();
    connection
        .execute("DELETE FROM moyai_schema_migrations WHERE version=70", [])
        .unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_v70_marker BEFORE INSERT ON moyai_schema_migrations
            WHEN NEW.version=70 BEGIN SELECT RAISE(ABORT,'fixture V70 marker failure'); END;",
        )
        .unwrap();
    let before = text_snapshot(
        &connection,
        "SELECT json_group_array(json_array(version,name)) FROM (SELECT * FROM moyai_schema_migrations ORDER BY version)",
    );
    assert!(
        run_clm_checkpoint(&connection)
            .unwrap_err()
            .to_string()
            .contains("fixture V70 marker failure")
    );
    assert_eq!(
        before,
        text_snapshot(
            &connection,
            "SELECT json_group_array(json_array(version,name)) FROM (SELECT * FROM moyai_schema_migrations ORDER BY version)"
        )
    );
    connection
        .execute_batch("DROP TRIGGER fail_v70_marker")
        .unwrap();
    run_to_current(&connection).unwrap();
    assert!(schema_migration_has_exact_name(&connection, 70, "clm_checkpoint").unwrap());
}

#[test]
fn v70_checkpoint_decoder_keeps_old_shapes_and_rejects_invalid_clm_payloads() {
    for layout in ["legacy_prefix", "user_anchored_checkpoint"] {
        let value = serde_json::json!({"kind":"compaction", "mode":"automatic", "layout":layout,
                "preserved_user_messages":[], "summary":"old summary", "replacement_item_ids":[]});
        let decoded = decode_current_compaction_payload("old", value.as_object().unwrap()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
    let checkpoint = crate::protocol::ClmCheckpoint::from_messages(
        &[crate::llm::ModelMessage::User {
            content: "task".into(),
        }],
        1,
    )
    .unwrap();
    let value = serde_json::json!({"kind":"compaction", "mode":"automatic", "layout":"clm_checkpoint",
            "clm_checkpoint":checkpoint, "preserved_user_messages":[], "summary":"Context updated", "replacement_item_ids":[]});
    assert!(decode_current_compaction_payload("valid", value.as_object().unwrap()).is_ok());
    for corruption in ["missing", "version", "authority", "layout", "anchors"] {
        let mut modified = value.clone();
        match corruption {
            "missing" => {
                modified.as_object_mut().unwrap().remove("clm_checkpoint");
            }
            "version" => modified["clm_checkpoint"]["version"] = serde_json::json!(9),
            "authority" => {
                modified["clm_checkpoint"]["messages"][0]["role"] = serde_json::json!("system")
            }
            "layout" => modified["layout"] = serde_json::json!("legacy_prefix"),
            "anchors" => modified["preserved_user_messages"] = serde_json::json!(["duplicate"]),
            _ => unreachable!(),
        }
        assert!(
            decode_current_compaction_payload("invalid", modified.as_object().unwrap()).is_err(),
            "{corruption}"
        );
    }
}

#[test]
fn v69_upgrade_rejects_corrupt_legacy_snapshots_without_changing_schema() {
    for corruption in [
        None,
        Some(r#"{"profile":"openai_compatible","api_key_env":"SIDE_KEY","extra_headers":{}}"#),
        Some(
            r#"{"profile":"openai_compatible","api_key_env":null,"extra_headers":{"X-Test":"value"}}"#,
        ),
    ] {
        let (connection, _, conversation, binding) = v69_predecessor_fixture(68);
        connection
            .execute(
                "UPDATE sessions SET provider_connection_json=?2 WHERE id=?1",
                params![conversation, corruption],
            )
            .unwrap();
        let before = v69_side_snapshot(&connection, &binding);
        let schema =
            normalized_schema_objects_for_table(&connection, "side_chat_bindings").unwrap();
        run_to_current(&connection)
            .expect_err("invalid released Side snapshot must not be repaired by V69");
        assert_eq!(v69_side_snapshot(&connection, &binding), before);
        assert_eq!(
            normalized_schema_objects_for_table(&connection, "side_chat_bindings").unwrap(),
            schema
        );
        assert!(
            !schema_migration_applied(&connection, SIDE_CHAT_API_KEY_REFERENCES_VERSION).unwrap()
        );
    }
}

#[test]
fn v69_failed_marker_write_rolls_back_both_replaced_triggers_and_data() {
    let (connection, _, _, binding) = v69_predecessor_fixture(68);
    let before = v69_side_snapshot(&connection, &binding);
    let schema = normalized_schema_objects_for_table(&connection, "side_chat_bindings").unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_v69_marker BEFORE INSERT ON moyai_schema_migrations
             WHEN NEW.version=69 BEGIN SELECT RAISE(ABORT,'fixture marker write failure'); END;",
        )
        .unwrap();
    let error = run_side_chat_api_key_references(&connection)
        .expect_err("late marker failure must roll back V69 DDL");
    assert!(error.to_string().contains("fixture marker write failure"));
    assert_eq!(
        normalized_schema_objects_for_table(&connection, "side_chat_bindings").unwrap(),
        schema
    );
    assert_eq!(v69_side_snapshot(&connection, &binding), before);
    assert!(!schema_migration_applied(&connection, SIDE_CHAT_API_KEY_REFERENCES_VERSION).unwrap());
    connection
        .execute_batch("DROP TRIGGER fail_v69_marker")
        .unwrap();
    run_to_current(&connection).expect("V69 succeeds after external storage failure is removed");
}

#[test]
fn v66_reopen_keeps_exact_side_schema_validation_and_rejects_route_trigger_tampering() {
    let connection = Connection::open_in_memory().expect("database");
    run_to_current(&connection).expect("current schema");
    run_to_current(&connection).expect("exact forward schema reopens");
    connection
        .execute_batch("DROP TRIGGER validate_side_chat_route_kind_before_update")
        .unwrap();
    assert!(
        run_to_current(&connection)
            .expect_err("missing route guard must not be accepted")
            .to_string()
            .contains("canonical system-prompt schema")
    );
}

#[test]
fn v61_reopen_rejects_transition_trigger_tampering() {
    let connection = Connection::open_in_memory().expect("database");
    run_to_current(&connection).expect("fresh V61 schema");
    connection
        .execute_batch("DROP TRIGGER validate_side_chat_binding_before_update")
        .expect("tamper V61 transition trigger");

    let error = run(&connection).expect_err("stale V61 schema must fail closed");
    assert!(
        error.to_string().contains("canonical system-prompt schema"),
        "unexpected V61 schema audit error: {error}"
    );
}
