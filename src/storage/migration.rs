use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use ulid::Ulid;

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
const V38_REMOVE_AUTO_REVIEW_ACCESS_MODE: &str =
    include_str!("../../migrations/V38__remove_auto_review_access_mode.sql");
const V39_TERMINAL_OUTCOME_CUTOVER: &str =
    include_str!("../../migrations/V39__terminal_outcome_cutover.sql");
const V40_FLATTEN_SESSION_SPAWN_EDGES: &str =
    include_str!("../../migrations/V40__flatten_session_spawn_edges.sql");
const V41_INDEXED_COLLABORATION_MODE_LOOKUP: &str =
    include_str!("../../migrations/V41__indexed_collaboration_mode_lookup.sql");
const V42_TYPED_HISTORY_SCOPE: &str = include_str!("../../migrations/V42__typed_history_scope.sql");
const V43_INDEXED_INTERNAL_FILE_OWNERSHIP: &str =
    include_str!("../../migrations/V43__indexed_internal_file_ownership.sql");
const V44_UNIQUE_TURN_TERMINAL: &str =
    include_str!("../../migrations/V44__unique_turn_terminal.sql");
const V45_RESTORE_AUTO_REVIEW_ACCESS_MODE: &str =
    include_str!("../../migrations/V45__restore_auto_review_access_mode.sql");
const V46_CODEX_COMPACTION_CHECKPOINT: &str =
    include_str!("../../migrations/V46__codex_compaction_checkpoint.sql");
const V47_RECURSIVE_SESSION_SPAWN_EDGES: &str =
    include_str!("../../migrations/V47__recursive_session_spawn_edges.sql");
const V48_AGENT_OWNER_RESUME_REQUESTS: &str =
    include_str!("../../migrations/V48__agent_owner_resume_requests.sql");
const V49_AGENT_TREE_STOP_FENCES: &str =
    include_str!("../../migrations/V49__agent_tree_stop_fences.sql");
const V50_DURABLE_AGENT_MAILBOX: &str =
    include_str!("../../migrations/V50__durable_agent_mailbox.sql");
const V51_DURABLE_TURN_INPUT_QUEUE: &str =
    include_str!("../../migrations/V51__durable_turn_input_queue.sql");
const V52_HARNESS_TURN_IDENTITY: &str =
    include_str!("../../migrations/V52__harness_turn_identity.sql");
const V53_AGENT_TRIGGER_TURN_CLAIMS: &str =
    include_str!("../../migrations/V53__agent_trigger_turn_claims.sql");
const LEGACY_PLANNER_CUTOVER_VERSION: i64 = 32;
const CANONICAL_PROTOCOL_STORAGE_VERSION: i64 = 33;
const DROP_SESSIONS_MEMORY_MODE_VERSION: i64 = 34;
const DROP_SESSIONS_AWAITING_USER_STATUS_VERSION: i64 = 35;
const DROP_LEGACY_REASONING_ITEMS_VERSION: i64 = 36;
const RAW_TOOL_CALL_HISTORY_VERSION: i64 = 37;
const REMOVE_AUTO_REVIEW_ACCESS_MODE_VERSION: i64 = 38;
const TERMINAL_OUTCOME_CUTOVER_VERSION: i64 = 39;
const FLATTEN_SESSION_SPAWN_EDGES_VERSION: i64 = 40;
const INDEXED_COLLABORATION_MODE_LOOKUP_VERSION: i64 = 41;
const TYPED_HISTORY_SCOPE_VERSION: i64 = 42;
const INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION: i64 = 43;
const UNIQUE_TURN_TERMINAL_VERSION: i64 = 44;
const RESTORE_AUTO_REVIEW_ACCESS_MODE_VERSION: i64 = 45;
const CODEX_COMPACTION_CHECKPOINT_VERSION: i64 = 46;
const RECURSIVE_SESSION_SPAWN_EDGES_VERSION: i64 = 47;
const AGENT_OWNER_RESUME_REQUESTS_VERSION: i64 = 48;
const AGENT_TREE_STOP_FENCES_VERSION: i64 = 49;
const DURABLE_AGENT_MAILBOX_VERSION: i64 = 50;
const DURABLE_TURN_INPUT_QUEUE_VERSION: i64 = 51;
const HARNESS_TURN_IDENTITY_VERSION: i64 = 52;
const AGENT_TRIGGER_TURN_CLAIMS_VERSION: i64 = 53;
const CODEX_COMPACTION_CHECKPOINT_NAME: &str = "codex_compaction_checkpoint";
const RECURSIVE_SESSION_SPAWN_EDGES_NAME: &str = "recursive_session_spawn_edges";
const AGENT_OWNER_RESUME_REQUESTS_NAME: &str = "agent_owner_resume_requests";
const AGENT_TREE_STOP_FENCES_NAME: &str = "agent_tree_stop_fences";
const DURABLE_AGENT_MAILBOX_NAME: &str = "durable_agent_mailbox";
const DURABLE_TURN_INPUT_QUEUE_NAME: &str = "durable_turn_input_queue";
const HARNESS_TURN_IDENTITY_NAME: &str = "harness_turn_identity";
const AGENT_TRIGGER_TURN_CLAIMS_NAME: &str = "agent_trigger_turn_claims";
const COMPACTION_CHECKPOINT_MIGRATION_PAGE_SIZE: usize = 200;
const SESSION_STATUS_DOMAIN: &[&str] = &["idle", "running", "completed", "cancelled", "failed"];
const SESSION_ACCESS_MODE_DOMAIN: &[&str] = &["default", "auto_review", "full_access"];
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
    if schema_migration_applied(connection, AGENT_TRIGGER_TURN_CLAIMS_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, HARNESS_TURN_IDENTITY_VERSION)? {
        // V53 has no recoverable backfill: it starts recording exact wake ownership for
        // admissions made after this endpoint.  Validate the complete V52 database before
        // creating its marker or schema so a pre-existing corruption leaves the database at the
        // exact V52 endpoint instead of committing a misleading partial upgrade.
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        run_agent_trigger_turn_claims(connection)?;
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DURABLE_TURN_INPUT_QUEUE_VERSION)? {
        run_harness_turn_identity(connection)?;
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)? {
        run_durable_turn_input_queue(connection)?;
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, AGENT_TREE_STOP_FENCES_VERSION)? {
        run_durable_agent_mailbox(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, AGENT_OWNER_RESUME_REQUESTS_VERSION)? {
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, RECURSIVE_SESSION_SPAWN_EDGES_VERSION)? {
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, CODEX_COMPACTION_CHECKPOINT_VERSION)? {
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, RESTORE_AUTO_REVIEW_ACCESS_MODE_VERSION)? {
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, UNIQUE_TURN_TERMINAL_VERSION)? {
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION)? {
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, TYPED_HISTORY_SCOPE_VERSION)? {
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_schema(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, INDEXED_COLLABORATION_MODE_LOOKUP_VERSION)? {
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, FLATTEN_SESSION_SPAWN_EDGES_VERSION)? {
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, TERMINAL_OUTCOME_CUTOVER_VERSION)? {
        run_flatten_session_spawn_edges(connection)?;
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, REMOVE_AUTO_REVIEW_ACCESS_MODE_VERSION)? {
        run_terminal_outcome_cutover(connection)?;
        run_flatten_session_spawn_edges(connection)?;
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, RAW_TOOL_CALL_HISTORY_VERSION)? {
        run_remove_auto_review_access_mode(connection)?;
        run_terminal_outcome_cutover(connection)?;
        run_flatten_session_spawn_edges(connection)?;
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DROP_LEGACY_REASONING_ITEMS_VERSION)? {
        run_raw_tool_call_history_migration(connection)?;
        run_remove_auto_review_access_mode(connection)?;
        run_terminal_outcome_cutover(connection)?;
        run_flatten_session_spawn_edges(connection)?;
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DROP_SESSIONS_AWAITING_USER_STATUS_VERSION)? {
        run_drop_legacy_reasoning_items(connection)?;
        run_raw_tool_call_history_migration(connection)?;
        run_remove_auto_review_access_mode(connection)?;
        run_terminal_outcome_cutover(connection)?;
        run_flatten_session_spawn_edges(connection)?;
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, DROP_SESSIONS_MEMORY_MODE_VERSION)? {
        run_drop_sessions_awaiting_user_status(connection)?;
        run_drop_legacy_reasoning_items(connection)?;
        run_raw_tool_call_history_migration(connection)?;
        run_remove_auto_review_access_mode(connection)?;
        run_terminal_outcome_cutover(connection)?;
        run_flatten_session_spawn_edges(connection)?;
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
        validate_canonical_protocol_storage(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, CANONICAL_PROTOCOL_STORAGE_VERSION)? {
        run_drop_sessions_memory_mode(connection)?;
        run_drop_sessions_awaiting_user_status(connection)?;
        run_drop_legacy_reasoning_items(connection)?;
        run_raw_tool_call_history_migration(connection)?;
        run_remove_auto_review_access_mode(connection)?;
        run_terminal_outcome_cutover(connection)?;
        run_flatten_session_spawn_edges(connection)?;
        run_indexed_collaboration_mode_lookup(connection)?;
        run_typed_history_scope(connection)?;
        run_indexed_internal_file_ownership(connection)?;
        run_unique_turn_terminal(connection)?;
        run_restore_auto_review_access_mode(connection)?;
        run_codex_compaction_checkpoint(connection)?;
        run_recursive_session_spawn_edges(connection)?;
        run_agent_owner_resume_requests(connection)?;
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
    run_remove_auto_review_access_mode(connection)?;
    run_terminal_outcome_cutover(connection)?;
    run_flatten_session_spawn_edges(connection)?;
    run_indexed_collaboration_mode_lookup(connection)?;
    run_typed_history_scope(connection)?;
    run_indexed_internal_file_ownership(connection)?;
    run_unique_turn_terminal(connection)?;
    run_restore_auto_review_access_mode(connection)?;
    run_codex_compaction_checkpoint(connection)?;
    run_recursive_session_spawn_edges(connection)?;
    run_agent_owner_resume_requests(connection)?;
    validate_canonical_protocol_storage(connection)?;
    Ok(())
}

fn run_indexed_collaboration_mode_lookup(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = connection
        .execute_batch(V41_INDEXED_COLLABORATION_MODE_LOOKUP)
        .map_err(StorageError::from);
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn run_indexed_internal_file_ownership(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = connection
        .execute_batch(V43_INDEXED_INTERNAL_FILE_OWNERSHIP)
        .map_err(StorageError::from);
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn run_unique_turn_terminal(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = connection
        .execute_batch(V44_UNIQUE_TURN_TERMINAL)
        .map_err(StorageError::from);
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn run_typed_history_scope(connection: &Connection) -> Result<(), StorageError> {
    // V42 is the destructive owner cutover. Audit every durable payload before
    // rebuilding the parent history table; subsequent opens use only bounded
    // schema-shape validation.
    validate_v39_history_json(connection)?;
    validate_v42_pseudo_turn_projections(connection)?;
    if legacy_reasoning_projection_row_count(connection)? != 0 {
        return Err(StorageError::Message(
            "V42 cannot type history scope while retired reasoning rows remain".to_string(),
        ));
    }
    validate_flat_session_spawn_edge_data(connection)?;
    validate_terminal_outcome_storage(connection)?;
    validate_raw_tool_call_history(connection)?;

    let foreign_keys_enabled =
        connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))? != 0;
    if foreign_keys_enabled {
        connection.pragma_update(None, "foreign_keys", "OFF")?;
    }

    let migration_result = (|| {
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            connection.execute_batch(V42_TYPED_HISTORY_SCOPE)?;
            let foreign_key_errors = connection.query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_check",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            if foreign_key_errors != 0 {
                return Err(StorageError::Message(format!(
                    "V42 typed history-scope cutover produced {foreign_key_errors} foreign-key violation(s)"
                )));
            }
            Ok::<_, StorageError>(())
        })();
        match result {
            Ok(()) => connection
                .execute_batch("COMMIT")
                .map_err(StorageError::from),
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    })();

    let restore_result = if foreign_keys_enabled {
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(StorageError::from)
    } else {
        Ok(())
    };
    migration_result?;
    restore_result?;
    Ok(())
}

fn validate_v42_pseudo_turn_projections(connection: &Connection) -> Result<(), StorageError> {
    let invalid_mode_turns = connection.query_row(
        "WITH session_only_turns AS (
             SELECT session_id, turn_id,
                    SUM(json_extract(payload_json, '$.kind') = 'collaboration_mode_instruction') AS mode_count,
                    SUM(json_extract(payload_json, '$.kind') = 'inter_agent_communication') AS mail_count,
                    SUM(json_extract(payload_json, '$.kind') NOT IN (
                        'collaboration_mode_instruction', 'inter_agent_communication'
                    )) AS other_count
             FROM protocol_history_items
             GROUP BY session_id, turn_id
         )
         SELECT COUNT(*)
         FROM session_only_turns AS candidate
         WHERE candidate.mode_count > 0
           AND candidate.other_count = 0
           AND (
               candidate.mail_count > 0
               OR EXISTS (
                   SELECT 1 FROM protocol_runtime_events AS runtime_event
                   WHERE runtime_event.session_id = candidate.session_id
                     AND runtime_event.turn_id = candidate.turn_id
               )
               OR EXISTS (
                   SELECT 1 FROM protocol_turn_items AS turn_item
                   WHERE turn_item.session_id = candidate.session_id
                     AND turn_item.turn_id = candidate.turn_id
               )
           )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_mode_turns != 0 {
        return Err(StorageError::Message(format!(
            "V42 refused to retire {invalid_mode_turns} collaboration-mode pseudo-turn(s) with unexpected mail, runtime, or turn projections"
        )));
    }

    let invalid_mail_turns = connection.query_row(
        "WITH mail_only_turns AS (
             SELECT session_id, turn_id
             FROM protocol_history_items
             GROUP BY session_id, turn_id
             HAVING COUNT(*) > 0
                AND SUM(json_extract(payload_json, '$.kind') <> 'inter_agent_communication') = 0
         )
         SELECT COUNT(*)
         FROM mail_only_turns AS candidate
         WHERE NOT EXISTS (
                   SELECT 1 FROM sessions AS session
                   WHERE session.id = candidate.session_id
                     AND session.active_turn_id = candidate.turn_id
               )
           AND NOT EXISTS (
                   SELECT 1 FROM protocol_runtime_events AS terminal
                   WHERE terminal.session_id = candidate.session_id
                     AND terminal.turn_id = candidate.turn_id
                     AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
               )
           AND NOT EXISTS (
                   SELECT 1 FROM protocol_turn_items AS terminal
                   WHERE terminal.session_id = candidate.session_id
                     AND terminal.turn_id = candidate.turn_id
                     AND json_extract(terminal.payload_json, '$.kind') = 'terminal'
               )
           AND (
               EXISTS (
                   SELECT 1 FROM protocol_runtime_events AS runtime_event
                   WHERE runtime_event.session_id = candidate.session_id
                     AND runtime_event.turn_id = candidate.turn_id
                     AND json_extract(runtime_event.msg_json, '$.kind') <> 'inter_agent_communication_received'
               )
               OR EXISTS (
                   SELECT 1 FROM protocol_turn_items AS turn_item
                   WHERE turn_item.session_id = candidate.session_id
                     AND turn_item.turn_id = candidate.turn_id
                     AND json_extract(turn_item.payload_json, '$.kind') <> 'inter_agent_communication'
               )
           )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_mail_turns != 0 {
        return Err(StorageError::Message(format!(
            "V42 refused to retire {invalid_mail_turns} terminal-less mail-only pseudo-turn(s) with unknown projections"
        )));
    }
    Ok(())
}

fn run_flatten_session_spawn_edges(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_v40_detached_edges_are_inactive(connection)?;
        connection.execute_batch(V40_FLATTEN_SESSION_SPAWN_EDGES)?;
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

fn validate_v40_detached_edges_are_inactive(connection: &Connection) -> Result<(), StorageError> {
    let active_detachments = connection.query_row(
        "WITH ranked AS (
             SELECT edge.*,
                    ROW_NUMBER() OVER (
                        PARTITION BY edge.root_session_id
                        ORDER BY edge.created_at_ms ASC, edge.child_session_id ASC
                    ) AS retained_order
             FROM session_spawn_edges AS edge
             WHERE edge.parent_session_id = edge.root_session_id
               AND edge.child_session_id <> edge.root_session_id
               AND edge.task_name <> ''
               AND edge.task_name <> 'root'
               AND edge.task_name NOT GLOB '*[^a-z0-9_]*'
               AND edge.agent_path = '/root/' || edge.task_name
         ), detached AS (
             SELECT edge.*
             FROM session_spawn_edges AS edge
             LEFT JOIN ranked
               ON ranked.child_session_id = edge.child_session_id
             WHERE ranked.child_session_id IS NULL OR ranked.retained_order > 255
         )
         SELECT COUNT(*)
         FROM detached
         INNER JOIN sessions AS root ON root.id = detached.root_session_id
         INNER JOIN sessions AS parent ON parent.id = detached.parent_session_id
         INNER JOIN sessions AS child ON child.id = detached.child_session_id
         WHERE root.status = 'running'
            OR parent.status = 'running'
            OR child.status = 'running'
            OR root.active_run_id IS NOT NULL
            OR parent.active_run_id IS NOT NULL
            OR child.active_run_id IS NOT NULL
            OR root.active_turn_id IS NOT NULL
            OR parent.active_turn_id IS NOT NULL
            OR child.active_turn_id IS NOT NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if active_detachments != 0 {
        return Err(StorageError::Message(format!(
            "V40 cannot detach {active_detachments} non-flat or over-capacity agent edge(s) while the affected tree retains active run state"
        )));
    }
    Ok(())
}

fn run_terminal_outcome_cutover(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        canonicalize_terminal_outcome_storage(connection)?;
        connection.execute_batch(V39_TERMINAL_OUTCOME_CUTOVER)?;
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

type TerminalOwnerKey = (String, String, i64);

fn canonicalize_terminal_outcome_storage(connection: &Connection) -> Result<(), StorageError> {
    validate_v39_history_json(connection)?;
    let runtime_outcomes = canonicalize_runtime_terminal_outcomes(connection)?;
    canonicalize_turn_terminal_outcomes(connection, &runtime_outcomes)
}

fn validate_v39_history_json(connection: &Connection) -> Result<(), StorageError> {
    let mut statement = connection
        .prepare("SELECT id, payload_json FROM protocol_history_items ORDER BY id ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, payload_json) = row?;
        protocol_json_object(&payload_json, "history item", &id)?;
    }
    Ok(())
}

fn canonicalize_runtime_terminal_outcomes(
    connection: &Connection,
) -> Result<BTreeMap<TerminalOwnerKey, crate::protocol::TurnTerminalOutcome>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, session_id, turn_id, sequence_no, msg_json
         FROM protocol_runtime_events ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let stored = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let mut outcomes = BTreeMap::new();
    for (id, session_id, turn_id, sequence_no, msg_json) in stored {
        let message = protocol_json_object(&msg_json, "runtime event", &id)?;
        if json_kind(&message, "runtime event", &id)? != "turn_terminal" {
            continue;
        }
        let terminal = message
            .get("terminal")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                StorageError::Message(format!("V39 runtime terminal {id} has no terminal object"))
            })?;
        let outcome = terminal_outcome_from_runtime_object(terminal, &id)?;

        let mut current_terminal = terminal.clone();
        for retired in ["status", "finish_reason", "interruption_cause", "summary"] {
            current_terminal.remove(retired);
        }
        current_terminal.insert("outcome".to_string(), serde_json::to_value(&outcome)?);
        let durable = serde_json::from_value::<crate::session::DurableTurnTerminal>(
            serde_json::Value::Object(current_terminal),
        )
        .map_err(|error| {
            StorageError::Message(format!(
                "V39 runtime terminal {id} cannot satisfy the current terminal contract: {error}"
            ))
        })?;
        let canonical_json = serde_json::to_string(&serde_json::json!({
            "kind": "turn_terminal",
            "terminal": durable,
        }))?;
        connection.execute(
            "UPDATE protocol_runtime_events
             SET msg_json = ?1, payload_sha256 = ?2
             WHERE id = ?3",
            (&canonical_json, sha256_text(&canonical_json), &id),
        )?;
        outcomes.insert((session_id, turn_id, sequence_no), outcome);
    }
    Ok(outcomes)
}

fn canonicalize_turn_terminal_outcomes(
    connection: &Connection,
    runtime_outcomes: &BTreeMap<TerminalOwnerKey, crate::protocol::TurnTerminalOutcome>,
) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, session_id, turn_id, sequence_no, payload_json
         FROM protocol_turn_items ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let stored = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (id, session_id, turn_id, sequence_no, payload_json) in stored {
        let payload = protocol_json_object(&payload_json, "turn item", &id)?;
        if json_kind(&payload, "turn item", &id)? != "terminal" {
            continue;
        }
        let owner_key = (session_id, turn_id, sequence_no);
        let outcome = match runtime_outcomes.get(&owner_key) {
            Some(outcome) => outcome.clone(),
            None => terminal_outcome_from_turn_object(&payload, &id)?,
        };
        let canonical_json = serde_json::to_string(&serde_json::json!({
            "kind": "terminal",
            "outcome": outcome,
        }))?;
        connection.execute(
            "UPDATE protocol_turn_items
             SET payload_json = ?1, payload_sha256 = ?2
             WHERE id = ?3",
            (&canonical_json, sha256_text(&canonical_json), &id),
        )?;
    }
    Ok(())
}

fn terminal_outcome_from_runtime_object(
    terminal: &serde_json::Map<String, serde_json::Value>,
    id: &str,
) -> Result<crate::protocol::TurnTerminalOutcome, StorageError> {
    if let Some(value) = terminal.get("outcome") {
        reject_mixed_terminal_contract(
            terminal,
            &["status", "finish_reason", "interruption_cause", "summary"],
            "runtime terminal",
            id,
        )?;
        return decode_current_terminal_outcome(value, "runtime terminal", id);
    }
    let status = required_terminal_string(terminal, "status", "runtime terminal", id)?;
    let summary = required_terminal_string(terminal, "summary", "runtime terminal", id)?;
    let finish_reason =
        optional_terminal_string(terminal, "finish_reason", "runtime terminal", id)?;
    let cause = optional_terminal_cause(terminal, "interruption_cause", "runtime terminal", id)?;
    outcome_from_legacy_terminal(
        status,
        summary,
        finish_reason,
        true,
        cause,
        "runtime terminal",
        id,
    )
}

fn terminal_outcome_from_turn_object(
    terminal: &serde_json::Map<String, serde_json::Value>,
    id: &str,
) -> Result<crate::protocol::TurnTerminalOutcome, StorageError> {
    if let Some(value) = terminal.get("outcome") {
        reject_mixed_terminal_contract(
            terminal,
            &["status", "summary", "cause"],
            "turn terminal",
            id,
        )?;
        return decode_current_terminal_outcome(value, "turn terminal", id);
    }
    let status = required_terminal_string(terminal, "status", "turn terminal", id)?;
    let summary = required_terminal_string(terminal, "summary", "turn terminal", id)?;
    let cause = optional_terminal_cause(terminal, "cause", "turn terminal", id)?;
    outcome_from_legacy_terminal(status, summary, None, false, cause, "turn terminal", id)
}

fn outcome_from_legacy_terminal(
    status: &str,
    summary: &str,
    finish_reason: Option<&str>,
    requires_finish_reason: bool,
    cause: Option<crate::protocol::TurnInterruptionCause>,
    owner: &str,
    id: &str,
) -> Result<crate::protocol::TurnTerminalOutcome, StorageError> {
    use crate::protocol::TurnTerminalOutcome;
    let finish_reason_matches = |expected: &str, optional_for_completed: bool| {
        if requires_finish_reason {
            finish_reason == Some(expected) || (optional_for_completed && finish_reason.is_none())
        } else {
            true
        }
    };
    match status {
        "completed" if finish_reason_matches("stop", true) && cause.is_none() => {
            Ok(TurnTerminalOutcome::Completed)
        }
        "failed" if finish_reason_matches("error", false) && cause.is_none() => {
            Ok(TurnTerminalOutcome::Failed {
                error: summary.to_string(),
            })
        }
        "interrupted" if finish_reason_matches("cancelled", false) => {
            let cause = cause
                .or_else(|| legacy_interruption_cause(summary))
                .ok_or_else(|| {
                    StorageError::Message(format!(
                        "V39 {owner} {id} is interrupted but has neither a typed cause nor a uniquely recognized legacy summary"
                    ))
                })?;
            Ok(TurnTerminalOutcome::Interrupted { cause })
        }
        _ => Err(StorageError::Message(format!(
            "V39 {owner} {id} has contradictory legacy terminal fields"
        ))),
    }
}

fn legacy_interruption_cause(summary: &str) -> Option<crate::protocol::TurnInterruptionCause> {
    use crate::protocol::TurnInterruptionCause;
    match summary {
        "permission approval aborted by user" => Some(TurnInterruptionCause::ApprovalAborted),
        "run stopped by user" => Some(TurnInterruptionCause::UserStop),
        "agent interrupted" => Some(TurnInterruptionCause::AgentInterrupted),
        "agent tree stopped" => Some(TurnInterruptionCause::TreeStopped),
        _ => None,
    }
}

fn decode_current_terminal_outcome(
    value: &serde_json::Value,
    owner: &str,
    id: &str,
) -> Result<crate::protocol::TurnTerminalOutcome, StorageError> {
    let outcome = serde_json::from_value::<crate::protocol::TurnTerminalOutcome>(value.clone())
        .map_err(|error| {
            StorageError::Message(format!(
                "V39 {owner} {id} has an invalid terminal outcome: {error}"
            ))
        })?;
    if serde_json::to_value(&outcome)? != *value {
        return Err(StorageError::Message(format!(
            "V39 {owner} {id} has non-canonical terminal outcome fields"
        )));
    }
    Ok(outcome)
}

fn reject_mixed_terminal_contract(
    object: &serde_json::Map<String, serde_json::Value>,
    retired_fields: &[&str],
    owner: &str,
    id: &str,
) -> Result<(), StorageError> {
    let mixed = retired_fields
        .iter()
        .filter(|field| object.contains_key(**field))
        .copied()
        .collect::<Vec<_>>();
    if mixed.is_empty() {
        Ok(())
    } else {
        Err(StorageError::Message(format!(
            "V39 {owner} {id} mixes outcome with retired fields: {}",
            mixed.join(", ")
        )))
    }
}

fn required_terminal_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    owner: &str,
    id: &str,
) -> Result<&'a str, StorageError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| StorageError::Message(format!("V39 {owner} {id} has no string `{field}`")))
}

fn optional_terminal_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    owner: &str,
    id: &str,
) -> Result<Option<&'a str>, StorageError> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(StorageError::Message(format!(
            "V39 {owner} {id} has a non-string `{field}`"
        ))),
    }
}

fn optional_terminal_cause(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    owner: &str,
    id: &str,
) -> Result<Option<crate::protocol::TurnInterruptionCause>, StorageError> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                StorageError::Message(format!(
                    "V39 {owner} {id} has an invalid typed interruption cause: {error}"
                ))
            }),
    }
}

fn protocol_json_object(
    json: &str,
    owner: &str,
    id: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, StorageError> {
    let value = serde_json::from_str::<serde_json::Value>(json).map_err(|error| {
        StorageError::Message(format!(
            "V39 cannot inspect protocol {owner} {id}: invalid JSON: {error}"
        ))
    })?;
    let object = value.as_object().cloned().ok_or_else(|| {
        StorageError::Message(format!("V39 protocol {owner} {id} is not a JSON object"))
    })?;
    json_kind(&object, owner, id)?;
    Ok(object)
}

fn json_kind<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    owner: &str,
    id: &str,
) -> Result<&'a str, StorageError> {
    object
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            StorageError::Message(format!("V39 protocol {owner} {id} has no string `kind`"))
        })
}

fn run_remove_auto_review_access_mode(connection: &Connection) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration(
        connection,
        V38_REMOVE_AUTO_REVIEW_ACCESS_MODE,
        "V38 auto-review access mode removal migration",
    )
}

fn run_restore_auto_review_access_mode(connection: &Connection) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration(
        connection,
        V45_RESTORE_AUTO_REVIEW_ACCESS_MODE,
        "V45 auto-review access mode restoration migration",
    )
}

fn run_codex_compaction_checkpoint(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let _ = canonicalize_compaction_checkpoint_history(connection)?;
        validate_compaction_checkpoint_history(connection)?;
        connection.execute_batch(V46_CODEX_COMPACTION_CHECKPOINT)?;
        if !schema_migration_has_exact_name(
            connection,
            CODEX_COMPACTION_CHECKPOINT_VERSION,
            CODEX_COMPACTION_CHECKPOINT_NAME,
        )? {
            return Err(StorageError::Message(
                "V46 compaction checkpoint migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
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

fn run_recursive_session_spawn_edges(connection: &Connection) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration_action(
        connection,
        "V47 recursive session spawn-edge migration",
        |connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| {
                connection.execute_batch(V47_RECURSIVE_SESSION_SPAWN_EDGES)?;
                if !schema_migration_has_exact_name(
                    connection,
                    RECURSIVE_SESSION_SPAWN_EDGES_VERSION,
                    RECURSIVE_SESSION_SPAWN_EDGES_NAME,
                )? {
                    return Err(StorageError::Message(
                        "V47 recursive spawn-edge migration did not record its exact schema marker"
                            .to_string(),
                    ));
                }
                validate_recursive_session_spawn_edge_schema(connection)?;
                validate_recursive_session_spawn_edge_data(connection)?;
                let foreign_key_errors = connection.query_row(
                    "SELECT COUNT(*) FROM pragma_foreign_key_check",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                if foreign_key_errors != 0 {
                    return Err(StorageError::Message(format!(
                        "V47 recursive spawn-edge migration produced {foreign_key_errors} foreign-key violation(s)"
                    )));
                }
                Ok::<_, StorageError>(())
            })();
            match result {
                Ok(()) => connection
                    .execute_batch("COMMIT")
                    .map_err(StorageError::from),
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        },
    )
}

fn run_agent_owner_resume_requests(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, AGENT_OWNER_RESUME_REQUESTS_VERSION)? {
            connection.execute_batch(V48_AGENT_OWNER_RESUME_REQUESTS)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            AGENT_OWNER_RESUME_REQUESTS_VERSION,
            AGENT_OWNER_RESUME_REQUESTS_NAME,
        )? {
            return Err(StorageError::Message(
                "V48 owner-resume migration did not record its exact schema marker".to_string(),
            ));
        }
        if !schema_migration_applied(connection, AGENT_TREE_STOP_FENCES_VERSION)? {
            connection.execute_batch(V49_AGENT_TREE_STOP_FENCES)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            AGENT_TREE_STOP_FENCES_VERSION,
            AGENT_TREE_STOP_FENCES_NAME,
        )? {
            return Err(StorageError::Message(
                "V49 tree-stop fence migration did not record its exact schema marker".to_string(),
            ));
        }
        validate_agent_owner_resume_request_schema(connection)?;
        validate_agent_owner_resume_request_data(connection)?;
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    run_durable_agent_mailbox(connection)
}

fn run_durable_agent_mailbox(connection: &Connection) -> Result<(), StorageError> {
    run_foreign_keys_disabled_migration_action(
        connection,
        "V50 durable agent-mailbox migration",
        |connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| {
                if !schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)? {
                    connection.execute_batch(V50_DURABLE_AGENT_MAILBOX)?;
                }
                if !schema_migration_has_exact_name(
                    connection,
                    DURABLE_AGENT_MAILBOX_VERSION,
                    DURABLE_AGENT_MAILBOX_NAME,
                )? {
                    return Err(StorageError::Message(
                        "V50 durable agent-mailbox migration did not record its exact schema marker"
                            .to_string(),
                    ));
                }
                validate_durable_agent_mailbox_schema(connection)?;
                validate_durable_agent_mailbox_data(connection)?;
                let foreign_key_errors = connection.query_row(
                    "SELECT COUNT(*) FROM pragma_foreign_key_check",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                if foreign_key_errors != 0 {
                    return Err(StorageError::Message(format!(
                        "V50 durable agent-mailbox migration produced {foreign_key_errors} foreign-key violation(s)"
                    )));
                }
                Ok::<_, StorageError>(())
            })();
            let transaction_result = match result {
                Ok(()) => connection
                    .execute_batch("COMMIT")
                    .map_err(StorageError::from),
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(error)
                }
            };
            let pragma_result = connection
                .execute_batch("PRAGMA legacy_alter_table = OFF")
                .map_err(StorageError::from);
            match (transaction_result, pragma_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) => Err(error),
                (Ok(()), Err(error)) => Err(error),
                (Err(migration_error), Err(cleanup_error)) => Err(StorageError::Message(format!(
                    "V50 durable agent-mailbox migration failed: {migration_error}; legacy-alter-table cleanup failed: {cleanup_error}"
                ))),
            }
        },
    )?;
    run_durable_turn_input_queue(connection)
}

fn run_durable_turn_input_queue(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, DURABLE_TURN_INPUT_QUEUE_VERSION)? {
            connection.execute_batch(V51_DURABLE_TURN_INPUT_QUEUE)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            DURABLE_TURN_INPUT_QUEUE_VERSION,
            DURABLE_TURN_INPUT_QUEUE_NAME,
        )? {
            return Err(StorageError::Message(
                "V51 durable turn-input queue migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_durable_turn_input_queue_schema(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V51 durable turn-input queue migration produced {foreign_key_errors} foreign-key violation(s)"
            )));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => connection
            .execute_batch("COMMIT")
            .map_err(StorageError::from),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }?;
    run_harness_turn_identity(connection)
}

fn run_harness_turn_identity(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, HARNESS_TURN_IDENTITY_VERSION)? {
            connection.execute_batch(V52_HARNESS_TURN_IDENTITY)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            HARNESS_TURN_IDENTITY_VERSION,
            HARNESS_TURN_IDENTITY_NAME,
        )? {
            return Err(StorageError::Message(
                "V52 harness turn-identity migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V52 harness turn-identity migration produced {foreign_key_errors} foreign-key violation(s)"
            )));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => connection
            .execute_batch("COMMIT")
            .map_err(StorageError::from),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }?;
    run_agent_trigger_turn_claims(connection)
}

fn run_agent_trigger_turn_claims(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, AGENT_TRIGGER_TURN_CLAIMS_VERSION)? {
            connection.execute_batch(V53_AGENT_TRIGGER_TURN_CLAIMS)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            AGENT_TRIGGER_TURN_CLAIMS_VERSION,
            AGENT_TRIGGER_TURN_CLAIMS_NAME,
        )? {
            return Err(StorageError::Message(
                "V53 agent trigger-turn claim migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V53 agent trigger-turn claim migration produced {foreign_key_errors} foreign-key violation(s)"
            )));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => connection
            .execute_batch("COMMIT")
            .map_err(StorageError::from),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CompactionCheckpointMigrationStats {
    pages: usize,
    rows: usize,
    max_page_rows: usize,
}

fn canonicalize_compaction_checkpoint_history(
    connection: &Connection,
) -> Result<CompactionCheckpointMigrationStats, StorageError> {
    validate_v39_history_json(connection)?;
    let missing_append_order = connection
        .query_row(
            "SELECT history.id
             FROM protocol_history_items AS history
             LEFT JOIN protocol_item_append_order AS append_order
               ON append_order.session_id = history.session_id
              AND append_order.source_kind = 'history_item'
              AND append_order.source_id = history.id
             WHERE json_extract(history.payload_json, '$.kind') = 'compaction'
               AND append_order.append_position IS NULL
             ORDER BY history.id ASC
             LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(id) = missing_append_order {
        return Err(StorageError::Message(format!(
            "V46 compaction history item {id} has no canonical append-order entry"
        )));
    }

    let mut stats = CompactionCheckpointMigrationStats::default();
    let mut after_append_position: Option<i64> = None;
    loop {
        let mut statement = connection.prepare(
            "SELECT history.id, history.session_id, history.payload_json,
                    history.payload_sha256, append_order.append_position
             FROM protocol_history_items AS history
             INNER JOIN protocol_item_append_order AS append_order
               ON append_order.session_id = history.session_id
              AND append_order.source_kind = 'history_item'
              AND append_order.source_id = history.id
             WHERE (?1 IS NULL OR append_order.append_position > ?1)
               AND json_extract(history.payload_json, '$.kind') = 'compaction'
             ORDER BY append_order.append_position ASC
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![
                    after_append_position,
                    i64::try_from(COMPACTION_CHECKPOINT_MIGRATION_PAGE_SIZE).unwrap_or(i64::MAX)
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
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if rows.is_empty() {
            break;
        }
        stats.pages = stats.pages.saturating_add(1);
        stats.rows = stats.rows.saturating_add(rows.len());
        stats.max_page_rows = stats.max_page_rows.max(rows.len());
        after_append_position = rows.last().map(|(.., append_position)| *append_position);

        for (id, session_id, payload_json, payload_sha256, append_position) in rows {
            let payload =
                serde_json::from_str::<serde_json::Value>(&payload_json).map_err(|error| {
                    StorageError::Message(format!(
                        "V46 cannot inspect protocol history item {id}: invalid JSON: {error}"
                    ))
                })?;
            let mut object = payload.as_object().cloned().ok_or_else(|| {
                StorageError::Message(format!(
                    "V46 compaction history item {id} is not a JSON object"
                ))
            })?;
            if payload_sha256 != sha256_text(&payload_json) {
                return Err(StorageError::Message(format!(
                    "V46 compaction history item {id} has a stale payload hash"
                )));
            }

            let has_layout = object.contains_key("layout");
            let has_preserved_messages = object.contains_key("preserved_user_messages");
            let legacy_payload = match (has_layout, has_preserved_messages) {
                (false, false) => {
                    object.insert(
                        "layout".to_string(),
                        serde_json::Value::String("legacy_prefix".to_string()),
                    );
                    object.insert(
                        "preserved_user_messages".to_string(),
                        serde_json::Value::Array(Vec::new()),
                    );
                    true
                }
                (true, true) => false,
                _ => {
                    return Err(StorageError::Message(format!(
                        "V46 compaction history item {id} mixes legacy and current checkpoint fields"
                    )));
                }
            };

            let mut current = decode_current_compaction_payload(&id, &object)?;
            if legacy_payload {
                let crate::protocol::HistoryItemPayload::Compaction {
                    layout,
                    preserved_user_messages,
                    replacement_item_ids,
                    ..
                } = &mut current
                else {
                    unreachable!("current payload was decoded as compaction");
                };
                let recovered = recover_legacy_compaction_user_messages(
                    connection,
                    &id,
                    &session_id,
                    append_position,
                    replacement_item_ids,
                )?;
                if !recovered.is_empty() {
                    *layout = crate::protocol::CompactionLayout::UserAnchoredCheckpoint;
                    *preserved_user_messages = recovered;
                }
            }
            let canonical_json = serde_json::to_string(&current)?;
            connection.execute(
                "UPDATE protocol_history_items
                 SET payload_json = ?1, payload_sha256 = ?2
                 WHERE id = ?3",
                (&canonical_json, sha256_text(&canonical_json), &id),
            )?;
        }
    }
    Ok(stats)
}

fn recover_legacy_compaction_user_messages(
    connection: &Connection,
    root_id: &str,
    session_id: &str,
    root_append_position: i64,
    replacement_item_ids: &[crate::protocol::HistoryItemId],
) -> Result<Vec<String>, StorageError> {
    let mut budget = crate::context::context_window::CompactionUserMessageBudget::new();
    'replacements: for replacement_item_id in replacement_item_ids.iter().rev() {
        let item_id = replacement_item_id.to_string();
        let row = connection
            .query_row(
                "SELECT history.session_id, history.payload_json, history.payload_sha256,
                        append_order.append_position
                 FROM protocol_history_items AS history
                 LEFT JOIN protocol_item_append_order AS append_order
                   ON append_order.session_id = history.session_id
                  AND append_order.source_kind = 'history_item'
                  AND append_order.source_id = history.id
                 WHERE history.id = ?1",
                [&item_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((replacement_session_id, payload_json, payload_sha256, append_position)) = row
        else {
            return Err(StorageError::Message(format!(
                "V46 legacy compaction history item {root_id} references missing replacement item {item_id}"
            )));
        };
        if replacement_session_id != session_id {
            return Err(StorageError::Message(format!(
                "V46 legacy compaction history item {root_id} references cross-session replacement item {item_id}"
            )));
        }
        let Some(append_position) = append_position else {
            return Err(StorageError::Message(format!(
                "V46 legacy compaction history item {root_id} references item {item_id} without a canonical append-order entry"
            )));
        };
        if append_position >= root_append_position {
            return Err(StorageError::Message(format!(
                "V46 legacy compaction history item {root_id} has a replacement cycle or forward reference through item {item_id}"
            )));
        }
        if payload_sha256 != sha256_text(&payload_json) {
            return Err(StorageError::Message(format!(
                "V46 legacy compaction history item {root_id} references item {item_id} with a stale payload hash"
            )));
        }
        let payload =
            serde_json::from_str::<crate::protocol::HistoryItemPayload>(&payload_json).map_err(
                |error| {
                    StorageError::Message(format!(
                        "V46 legacy compaction history item {root_id} cannot decode replacement item {item_id}: {error}"
                    ))
                },
            )?;

        let messages = match payload {
            crate::protocol::HistoryItemPayload::UserTurn { content, .. }
            | crate::protocol::HistoryItemPayload::SteerTurn { content, .. } => {
                let text = content
                    .iter()
                    .filter_map(|part| match part {
                        crate::protocol::ContentPart::Text { text } => Some(text.as_str()),
                        crate::protocol::ContentPart::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![text]
                }
            }
            crate::protocol::HistoryItemPayload::Compaction {
                layout,
                preserved_user_messages,
                ..
            } if layout.appends_checkpoint() => preserved_user_messages,
            _ => Vec::new(),
        };
        for message in messages
            .into_iter()
            .rev()
            .filter(|message| !message.trim().is_empty())
        {
            if !budget.push_newest(message) {
                break 'replacements;
            }
        }
    }
    Ok(budget.finish())
}

fn decode_current_compaction_payload(
    id: &str,
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<crate::protocol::HistoryItemPayload, StorageError> {
    const ALLOWED_FIELDS: &[&str] = &[
        "kind",
        "mode",
        "layout",
        "preserved_user_messages",
        "summary",
        "replacement_item_ids",
    ];
    let unexpected = object
        .keys()
        .filter(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(StorageError::Message(format!(
            "V46 compaction history item {id} contains unexpected fields: {}",
            unexpected.join(", ")
        )));
    }
    for field in ALLOWED_FIELDS {
        if !object.contains_key(*field) {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} has no `{field}` field"
            )));
        }
    }

    let current = serde_json::from_value::<crate::protocol::HistoryItemPayload>(
        serde_json::Value::Object(object.clone()),
    )
    .map_err(|error| {
        StorageError::Message(format!(
            "V46 compaction history item {id} violates the current payload contract: {error}"
        ))
    })?;
    let crate::protocol::HistoryItemPayload::Compaction {
        layout,
        preserved_user_messages,
        ..
    } = &current
    else {
        return Err(StorageError::Message(format!(
            "V46 compaction history item {id} decoded as another payload kind"
        )));
    };
    if matches!(layout, crate::protocol::CompactionLayout::LegacyPrefix)
        && !preserved_user_messages.is_empty()
    {
        return Err(StorageError::Message(format!(
            "V46 legacy-prefix compaction history item {id} retains user-anchor messages"
        )));
    }
    Ok(current)
}

fn validate_compaction_checkpoint_history(connection: &Connection) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, session_id, payload_json, payload_sha256
         FROM protocol_history_items
         ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (id, session_id, payload_json, payload_sha256) = row?;
        let payload = serde_json::from_str::<serde_json::Value>(&payload_json).map_err(|error| {
            StorageError::Message(format!(
                "V46 marker exists but protocol history item {id} contains invalid JSON: {error}"
            ))
        })?;
        let Some(object) = payload.as_object() else {
            continue;
        };
        if object.get("kind").and_then(serde_json::Value::as_str) != Some("compaction") {
            continue;
        }
        if payload_sha256 != sha256_text(&payload_json) {
            return Err(StorageError::Message(format!(
                "V46 marker exists but compaction history item {id} has a stale payload hash"
            )));
        }
        let current = decode_current_compaction_payload(&id, object)?;
        validate_current_compaction_storage(connection, &id, &session_id, &current)?;
    }
    Ok(())
}

fn validate_current_compaction_storage(
    connection: &Connection,
    id: &str,
    session_id: &str,
    payload: &crate::protocol::HistoryItemPayload,
) -> Result<(), StorageError> {
    let crate::protocol::HistoryItemPayload::Compaction {
        preserved_user_messages,
        replacement_item_ids,
        ..
    } = payload
    else {
        return Err(StorageError::Message(format!(
            "V46 compaction history item {id} decoded as another payload kind"
        )));
    };
    let anchor_tokens = preserved_user_messages
        .iter()
        .map(|message| crate::context::context_window::estimate_text_tokens(message))
        .fold(0usize, usize::saturating_add);
    if anchor_tokens > crate::context::context_window::COMPACTION_USER_MESSAGE_MAX_TOKENS {
        return Err(StorageError::Message(format!(
            "V46 compaction history item {id} retains {anchor_tokens} estimated user-anchor tokens, exceeding the 20000-token checkpoint bound"
        )));
    }

    let root_append_position = connection
        .query_row(
            "SELECT append_position
             FROM protocol_item_append_order
             WHERE session_id = ?1
               AND source_kind = 'history_item'
               AND source_id = ?2",
            params![session_id, id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "V46 compaction history item {id} has no canonical append-order entry"
            ))
        })?;

    let mut unique_replacements = BTreeSet::new();
    for replacement_item_id in replacement_item_ids {
        let replacement_item_id = replacement_item_id.to_string();
        if replacement_item_id == id {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} replaces itself"
            )));
        }
        if !unique_replacements.insert(replacement_item_id.clone()) {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} repeats replacement item {replacement_item_id}"
            )));
        }
        let replacement = connection
            .query_row(
                "SELECT history.session_id, history.payload_json, history.payload_sha256,
                        append_order.append_position
                 FROM protocol_history_items AS history
                 LEFT JOIN protocol_item_append_order AS append_order
                   ON append_order.session_id = history.session_id
                  AND append_order.source_kind = 'history_item'
                  AND append_order.source_id = history.id
                 WHERE history.id = ?1",
                [&replacement_item_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            replacement_session_id,
            replacement_payload_json,
            replacement_payload_sha256,
            replacement_append_position,
        )) = replacement
        else {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} references missing or cross-session replacement item {replacement_item_id}"
            )));
        };
        if replacement_session_id != session_id {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} references missing or cross-session replacement item {replacement_item_id}"
            )));
        }
        if replacement_payload_sha256 != sha256_text(&replacement_payload_json) {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} references replacement item {replacement_item_id} with a stale payload hash"
            )));
        }
        let Some(replacement_append_position) = replacement_append_position else {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} references replacement item {replacement_item_id} without a canonical append-order entry"
            )));
        };
        if replacement_append_position >= root_append_position {
            return Err(StorageError::Message(format!(
                "V46 compaction history item {id} has a replacement cycle or forward reference through item {replacement_item_id}"
            )));
        }
    }
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
    run_foreign_keys_disabled_migration_action(
        connection,
        "V33 canonical protocol storage migration",
        |connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            backfill_legacy_protocol_storage(connection)?;
            connection.execute_batch(V33_CANONICAL_PROTOCOL_STORAGE)?;
            connection.execute_batch("COMMIT")?;
            Ok(())
        },
    )
}

#[derive(Debug, Clone)]
struct LegacyMessageRow {
    id: String,
    session_id: String,
    parent_message_id: Option<String>,
    role: String,
    metadata_json: String,
    created_at_ms: i64,
}

fn backfill_legacy_protocol_storage(connection: &Connection) -> Result<(), StorageError> {
    if !table_exists(connection, "messages")? || !table_exists(connection, "message_parts")? {
        return Err(StorageError::Message(
            "V33 requires the released messages and message_parts tables before cutover"
                .to_string(),
        ));
    }

    let mut message_statement = connection.prepare(
        "SELECT id, session_id, parent_message_id, role, metadata_json, created_at_ms
         FROM messages ORDER BY session_id ASC, sequence_no ASC, id ASC",
    )?;
    let messages = message_statement
        .query_map([], |row| {
            Ok(LegacyMessageRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                parent_message_id: row.get(2)?,
                role: row.get(3)?,
                metadata_json: row.get(4)?,
                created_at_ms: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(message_statement);

    let mut message_turns = HashMap::<String, String>::new();
    let mut dual_written_messages = BTreeSet::<String>::new();
    let mut response_ids = HashMap::<String, String>::new();
    let mut history_statement = connection.prepare(
        "SELECT turn_id, payload_json FROM protocol_history_items
         ORDER BY created_at_ms ASC, sequence_no ASC, id ASC",
    )?;
    let history_rows = history_statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in history_rows {
        let (turn_id, payload_json) = row?;
        let payload: serde_json::Value = serde_json::from_str(&payload_json).map_err(|error| {
            StorageError::Message(format!(
                "V33 cannot inspect canonical history while backfilling legacy messages: {error}"
            ))
        })?;
        let Some(object) = payload.as_object() else {
            continue;
        };
        let Some(message_id) = object
            .get("message_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if let Some(existing_turn) = message_turns.insert(message_id.to_string(), turn_id.clone())
            && existing_turn != turn_id
        {
            return Err(StorageError::Message(format!(
                "V33 legacy message {message_id} is projected into more than one canonical turn"
            )));
        }
        dual_written_messages.insert(message_id.to_string());
        if let Some(response_id) = object
            .get("response_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        {
            response_ids.insert(message_id.to_string(), response_id.to_string());
        }
    }
    drop(history_statement);

    let mut latest_user_turn = HashMap::<String, String>::new();
    for message in &messages {
        let turn_id = if let Some(turn_id) = message_turns.get(&message.id) {
            turn_id.clone()
        } else if message.role == "assistant" {
            message
                .parent_message_id
                .as_ref()
                .and_then(|parent| message_turns.get(parent))
                .cloned()
                .or_else(|| latest_user_turn.get(&message.session_id).cloned())
                .unwrap_or_else(new_migration_protocol_id)
        } else if message.role == "user" {
            new_migration_protocol_id()
        } else {
            return Err(StorageError::Message(format!(
                "V33 legacy message {} has unsupported role `{}`",
                message.id, message.role
            )));
        };
        message_turns.insert(message.id.clone(), turn_id.clone());
        if message.role == "user" {
            latest_user_turn.insert(message.session_id.clone(), turn_id);
        } else {
            response_ids
                .entry(message.id.clone())
                .or_insert_with(new_migration_protocol_id);
        }
    }

    normalize_dual_written_message_history(connection, &response_ids)?;

    for message in &messages {
        if dual_written_messages.contains(&message.id) {
            continue;
        }
        let turn_id = message_turns.get(&message.id).ok_or_else(|| {
            StorageError::Message(format!(
                "V33 did not allocate a turn for message {}",
                message.id
            ))
        })?;
        let parts = legacy_message_parts(connection, &message.id)?;
        let mut content = Vec::new();
        let mut prompt_dispatch = None;
        let mut deferred = Vec::new();
        for part in parts {
            match part.part_kind.as_str() {
                "text" => {
                    let inner = legacy_part_inner(&part, "Text")?;
                    let text = required_value_string(&inner, "text", &part.id, "V33 text part")?;
                    content.push(serde_json::json!({"kind": "text", "text": text}));
                }
                "image" => {
                    let inner = legacy_part_inner(&part, "Image")?;
                    content.push(serde_json::json!({"kind": "image", "image": inner}));
                }
                "prompt_dispatch" => {
                    if prompt_dispatch.is_some() {
                        return Err(StorageError::Message(format!(
                            "V33 message {} has more than one prompt-dispatch part",
                            message.id
                        )));
                    }
                    prompt_dispatch = Some(legacy_part_inner(&part, "PromptDispatch")?);
                }
                "reasoning" => {
                    // Raw reasoning was never a durable model-context contract and is retired by V36.
                    legacy_part_inner(&part, "Reasoning")?;
                }
                "error" | "request_diagnostics" | "tool_call" | "tool_result" | "diff_summary" => {
                    deferred.push(part)
                }
                other => {
                    return Err(StorageError::Message(format!(
                        "V33 message {} contains unsupported legacy part kind `{other}`",
                        message.id
                    )));
                }
            }
        }
        let payload = match message.role.as_str() {
            "user" => {
                let metadata: serde_json::Value = serde_json::from_str(&message.metadata_json)
                    .map_err(|error| {
                        StorageError::Message(format!(
                            "V33 user message {} has invalid metadata JSON: {error}",
                            message.id
                        ))
                    })?;
                let editor_context = legacy_enum_inner(&metadata, "User")
                    .and_then(|value| value.get("editor_context"))
                    .filter(|value| !value.is_null())
                    .cloned();
                let mut object = serde_json::Map::new();
                object.insert("kind".to_string(), serde_json::json!("user_turn"));
                object.insert("content".to_string(), serde_json::Value::Array(content));
                if let Some(dispatch) = prompt_dispatch {
                    object.insert("prompt_dispatch".to_string(), dispatch);
                }
                if let Some(editor_context) = editor_context {
                    object.insert("editor_context".to_string(), editor_context);
                }
                serde_json::Value::Object(object)
            }
            "assistant" => serde_json::json!({
                "kind": "assistant_message",
                "response_id": response_ids.get(&message.id).ok_or_else(|| StorageError::Message(
                    format!("V33 assistant message {} has no response identity", message.id)
                ))?,
                "content": content,
            }),
            _ => unreachable!(),
        };
        insert_v33_history_item(
            connection,
            &message.session_id,
            turn_id,
            message.created_at_ms,
            payload,
        )?;
        backfill_deferred_message_parts(
            connection,
            message,
            turn_id,
            response_ids.get(&message.id),
            &deferred,
        )?;
    }

    backfill_legacy_tool_and_file_evidence(connection, &message_turns, &response_ids)?;
    rebuild_protocol_append_order(connection)?;
    Ok(())
}

fn new_migration_protocol_id() -> String {
    Ulid::new().to_string()
}

#[derive(Debug, Clone)]
struct LegacyPartRow {
    id: String,
    part_kind: String,
    payload_json: String,
    created_at_ms: i64,
}

fn legacy_message_parts(
    connection: &Connection,
    message_id: &str,
) -> Result<Vec<LegacyPartRow>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, part_kind, payload_json, created_at_ms FROM message_parts
         WHERE message_id = ?1 ORDER BY sequence_no ASC, id ASC",
    )?;
    let rows = statement
        .query_map([message_id], |row| {
            Ok(LegacyPartRow {
                id: row.get(0)?,
                part_kind: row.get(1)?,
                payload_json: row.get(2)?,
                created_at_ms: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn legacy_part_inner(
    part: &LegacyPartRow,
    variant: &str,
) -> Result<serde_json::Value, StorageError> {
    let payload: serde_json::Value = serde_json::from_str(&part.payload_json).map_err(|error| {
        StorageError::Message(format!(
            "V33 legacy part {} contains invalid JSON: {error}",
            part.id
        ))
    })?;
    legacy_enum_inner(&payload, variant)
        .cloned()
        .ok_or_else(|| {
            StorageError::Message(format!(
                "V33 legacy part {} is not the expected {variant} payload",
                part.id
            ))
        })
}

fn legacy_enum_inner<'a>(
    value: &'a serde_json::Value,
    variant: &str,
) -> Option<&'a serde_json::Value> {
    value.as_object()?.get(variant)
}

fn required_value_string<'a>(
    value: &'a serde_json::Value,
    field: &str,
    owner_id: &str,
    owner: &str,
) -> Result<&'a str, StorageError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| StorageError::Message(format!("{owner} {owner_id} has no string `{field}`")))
}

fn insert_v33_history_item(
    connection: &Connection,
    session_id: &str,
    turn_id: &str,
    created_at_ms: i64,
    payload: serde_json::Value,
) -> Result<String, StorageError> {
    let sequence_no = connection.query_row(
        "SELECT COALESCE(MAX(sequence_no), -1) + 1 FROM (
             SELECT sequence_no FROM protocol_history_items WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL SELECT sequence_no FROM protocol_runtime_events WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL SELECT sequence_no FROM protocol_turn_items WHERE session_id = ?1 AND turn_id = ?2
         )",
        (session_id, turn_id),
        |row| row.get::<_, i64>(0),
    )?;
    let id = new_migration_protocol_id();
    let payload_json = serde_json::to_string(&payload)?;
    let payload_sha256 = sha256_text(&payload_json);
    connection.execute(
        "INSERT INTO protocol_history_items
         (id, session_id, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            id,
            session_id,
            turn_id,
            sequence_no,
            payload_json,
            payload_sha256,
            created_at_ms
        ],
    )?;
    connection.execute(
        "INSERT INTO protocol_item_append_order
         (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
         VALUES (?1, ?2, ?3, 'history_item', ?4, ?5)",
        rusqlite::params![session_id, turn_id, sequence_no, id, created_at_ms],
    )?;
    connection.execute(
        "INSERT INTO protocol_turn_sequence_allocators (session_id, turn_id, next_sequence_no)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id, turn_id) DO UPDATE SET
             next_sequence_no = MAX(protocol_turn_sequence_allocators.next_sequence_no,
                                    excluded.next_sequence_no)",
        rusqlite::params![session_id, turn_id, sequence_no + 1],
    )?;
    Ok(id)
}

fn normalize_dual_written_message_history(
    connection: &Connection,
    response_ids: &HashMap<String, String>,
) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, payload_json FROM protocol_history_items
         WHERE json_valid(payload_json)
           AND json_extract(payload_json, '$.kind') IN ('message', 'user_turn')
           AND json_type(payload_json, '$.message_id') = 'text'
         ORDER BY id ASC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (id, payload_json) in rows {
        let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
        let object = payload.as_object().ok_or_else(|| {
            StorageError::Message(format!("V33 history item {id} is not a JSON object"))
        })?;
        let message_id = required_value_string(&payload, "message_id", &id, "V33 history item")?;
        let content = object
            .get("content")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
        let canonical = match object.get("kind").and_then(serde_json::Value::as_str) {
            Some("message") => match object.get("role").and_then(serde_json::Value::as_str) {
                Some("assistant") => serde_json::json!({
                    "kind": "assistant_message",
                    "response_id": response_ids.get(message_id).ok_or_else(|| StorageError::Message(
                        format!("V33 dual-written assistant message {message_id} has no response identity")
                    ))?,
                    "content": content,
                }),
                Some("user") => serde_json::json!({"kind": "user_turn", "content": content}),
                role => {
                    return Err(StorageError::Message(format!(
                        "V33 history item {id} has unsupported legacy message role {role:?}"
                    )));
                }
            },
            Some("user_turn") => {
                let mut canonical = serde_json::Map::new();
                canonical.insert("kind".to_string(), serde_json::json!("user_turn"));
                canonical.insert("content".to_string(), content);
                for field in ["prompt_dispatch", "editor_context"] {
                    if let Some(value) = object.get(field).filter(|value| !value.is_null()) {
                        canonical.insert(field.to_string(), value.clone());
                    }
                }
                serde_json::Value::Object(canonical)
            }
            _ => continue,
        };
        let canonical_json = serde_json::to_string(&canonical)?;
        connection.execute(
            "UPDATE protocol_history_items SET payload_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
            (&canonical_json, sha256_text(&canonical_json), &id),
        )?;
    }
    Ok(())
}

fn backfill_deferred_message_parts(
    connection: &Connection,
    message: &LegacyMessageRow,
    turn_id: &str,
    response_id: Option<&String>,
    parts: &[LegacyPartRow],
) -> Result<(), StorageError> {
    for part in parts {
        let payload = match part.part_kind.as_str() {
            "error" => {
                let inner = legacy_part_inner(part, "Error")?;
                serde_json::json!({
                    "kind": "error",
                    "message": required_value_string(&inner, "message", &part.id, "V33 error part")?,
                })
            }
            "request_diagnostics" => serde_json::json!({
                "kind": "request_diagnostics",
                "diagnostics": legacy_part_inner(part, "RequestDiagnostics")?,
            }),
            "tool_call" => {
                let inner = legacy_part_inner(part, "ToolCall")?;
                let response_id = response_id.ok_or_else(|| {
                    StorageError::Message(format!(
                        "V33 tool-call part {} is not owned by an assistant response",
                        part.id
                    ))
                })?;
                let call_id =
                    required_value_string(&inner, "tool_call_id", &part.id, "V33 tool-call part")?;
                let tool_name =
                    required_value_string(&inner, "tool_name", &part.id, "V33 tool-call part")?;
                let arguments_json = required_value_string(
                    &inner,
                    "arguments_json",
                    &part.id,
                    "V33 tool-call part",
                )?;
                if legacy_tool_call_row_exists(connection, call_id)? {
                    // The released tool_calls row is the lossless owner of this dual-written
                    // projection and is materialized below with its exact provider fields.
                    continue;
                }
                serde_json::json!({
                    "kind": "tool_call",
                    "call_id": call_id,
                    "response_id": response_id,
                    "tool_name": tool_name,
                    "arguments_json": arguments_json,
                })
            }
            "tool_result" => {
                let inner = legacy_part_inner(part, "ToolResult")?;
                let call_id = required_value_string(
                    &inner,
                    "tool_call_id",
                    &part.id,
                    "V33 tool-result part",
                )?;
                let status =
                    required_value_string(&inner, "status", &part.id, "V33 tool-result part")?;
                let title =
                    required_value_string(&inner, "title", &part.id, "V33 tool-result part")?;
                let output_text =
                    required_value_string(&inner, "summary", &part.id, "V33 tool-result part")?;
                if legacy_tool_call_row_exists(connection, call_id)? {
                    // Avoid letting the summary-only message part mask the exact output,
                    // error, metadata and terminal timestamps retained by tool_calls.
                    continue;
                }
                serde_json::json!({
                    "kind": "tool_output",
                    "call_id": call_id,
                    "status": status,
                    "title": title,
                    "output_text": output_text,
                    "metadata": inner,
                    "success": inner.get("success").cloned().unwrap_or(serde_json::Value::Null),
                })
            }
            "diff_summary" => {
                let inner = legacy_part_inner(part, "DiffSummary")?;
                let call_id = inner
                    .get("tool_call_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        StorageError::Message(format!(
                            "V33 diff-summary part {} has no tool_call_id",
                            part.id
                        ))
                    })?;
                serde_json::json!({
                    "kind": "file_change",
                    "call_id": call_id,
                    "change_ids": inner.get("change_ids").cloned().unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
                    "changes": inner.get("changes").cloned().unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
                    "summary": required_value_string(&inner, "summary", &part.id, "V33 diff-summary part")?,
                })
            }
            _ => continue,
        };
        insert_v33_history_item(
            connection,
            &message.session_id,
            turn_id,
            part.created_at_ms,
            payload,
        )?;
    }
    Ok(())
}

fn legacy_tool_call_row_exists(
    connection: &Connection,
    call_id: &str,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_calls WHERE id = ?1)",
            [call_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(StorageError::from)
}

fn backfill_legacy_tool_and_file_evidence(
    connection: &Connection,
    message_turns: &HashMap<String, String>,
    response_ids: &HashMap<String, String>,
) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, session_id, message_id, tool_name, status, arguments_json, title,
                metadata_json, output_text, error_text, started_at_ms, finished_at_ms
         FROM tool_calls ORDER BY session_id ASC, started_at_ms ASC, id ASC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<i64>>(11)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (
        call_id,
        session_id,
        message_id,
        tool_name,
        status,
        arguments_json,
        title,
        metadata_json,
        output_text,
        error_text,
        started_at_ms,
        finished_at_ms,
    ) in rows
    {
        let turn_id = message_turns.get(&message_id).ok_or_else(|| {
            StorageError::Message(format!(
                "V33 tool call {call_id} references unknown message {message_id}"
            ))
        })?;
        let response_id = response_ids.get(&message_id).ok_or_else(|| {
            StorageError::Message(format!(
                "V33 tool call {call_id} is not owned by an assistant response"
            ))
        })?;
        let existing_calls = history_item_ids_for_call(connection, "tool_call", &call_id)?;
        let dual_written_tool_call_payload_json = match existing_calls.as_slice() {
            [] => {
                insert_v33_history_item(
                    connection,
                    &session_id,
                    turn_id,
                    started_at_ms,
                    serde_json::json!({
                        "kind": "tool_call",
                        "call_id": call_id,
                        "response_id": response_id,
                        "tool_name": tool_name,
                        "arguments_json": arguments_json,
                    }),
                )?;
                None
            }
            [history_id] => {
                let payload_json = connection.query_row(
                    "SELECT payload_json FROM protocol_history_items WHERE id = ?1",
                    [history_id],
                    |row| row.get::<_, String>(0),
                )?;
                let _: serde_json::Value = serde_json::from_str(&payload_json)?;
                let canonical_json = serde_json::to_string(&serde_json::json!({
                    "kind": "tool_call",
                    "call_id": call_id,
                    "response_id": response_id,
                    "tool_name": tool_name,
                    "arguments_json": arguments_json,
                }))?;
                connection.execute(
                    "UPDATE protocol_history_items SET payload_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
                    (&canonical_json, sha256_text(&canonical_json), history_id),
                )?;
                Some(payload_json)
            }
            _ => {
                return Err(StorageError::Message(format!(
                    "V33 tool call {call_id} has more than one canonical history owner"
                )));
            }
        };

        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_json).map_err(|error| {
                StorageError::Message(format!(
                    "V33 tool call {call_id} has invalid metadata JSON: {error}"
                ))
            })?;
        let success = match status.as_str() {
            "completed" => Some(true),
            "declined" | "cancelled" | "failed" => Some(false),
            "pending" | "running" => None,
            _ => {
                return Err(StorageError::Message(format!(
                    "V33 tool call {call_id} has unsupported status `{status}`"
                )));
            }
        };
        let canonical_title = title.clone().unwrap_or_else(|| tool_name.clone());
        let canonical_output = output_text
            .clone()
            .or_else(|| error_text.clone())
            .unwrap_or_default();
        let existing_outputs = history_item_ids_for_call(connection, "tool_output", &call_id)?;
        if existing_outputs.len() > 1 {
            return Err(StorageError::Message(format!(
                "V33 tool call {call_id} has more than one canonical output owner"
            )));
        }
        let dual_written_history_payload_json = existing_outputs
            .first()
            .map(|history_id| {
                connection.query_row(
                    "SELECT payload_json FROM protocol_history_items WHERE id = ?1",
                    [history_id],
                    |row| row.get::<_, String>(0),
                )
            })
            .transpose()?;
        let lossless_legacy_evidence = serde_json::json!({
            "legacy_metadata_json": metadata_json,
            "legacy_metadata": metadata,
            "legacy_title": title,
            "legacy_output_text": output_text,
            "legacy_error_text": error_text,
            "legacy_started_at_ms": started_at_ms,
            "legacy_finished_at_ms": finished_at_ms,
            "dual_written_tool_call_payload_json": dual_written_tool_call_payload_json,
            "dual_written_history_payload_json": dual_written_history_payload_json,
        });
        let canonical_tool_output = serde_json::json!({
            "kind": "tool_output",
            "call_id": call_id,
            "status": status,
            "title": canonical_title,
            "output_text": canonical_output,
            "metadata": lossless_legacy_evidence,
            "success": success,
        });
        if let Some(history_id) = existing_outputs.first() {
            let canonical_json = serde_json::to_string(&canonical_tool_output)?;
            connection.execute(
                "UPDATE protocol_history_items
                 SET payload_json = ?1, payload_sha256 = ?2
                 WHERE id = ?3",
                (&canonical_json, sha256_text(&canonical_json), history_id),
            )?;
        } else {
            insert_v33_history_item(
                connection,
                &session_id,
                turn_id,
                finished_at_ms.unwrap_or(started_at_ms),
                canonical_tool_output,
            )?;
        }

        let mut changes = connection.prepare(
            "SELECT id, change_kind, path_before, path_after, summary_text, created_at_ms
             FROM file_changes WHERE tool_call_id = ?1 ORDER BY created_at_ms ASC, id ASC",
        )?;
        let change_rows = changes
            .query_map([&call_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(changes);
        for (change_id, change_kind, path_before, path_after, summary, created_at_ms) in change_rows
        {
            if canonical_history_contains_change(connection, &change_id)? {
                continue;
            }
            insert_v33_history_item(
                connection,
                &session_id,
                turn_id,
                created_at_ms,
                serde_json::json!({
                    "kind": "file_change",
                    "call_id": call_id,
                    "change_ids": [canonical_ulid_or_new(&change_id)],
                    "changes": [{
                        "change_id": canonical_ulid_or_new(&change_id),
                        "kind": change_kind,
                        "path_before": path_before,
                        "path_after": path_after,
                        "summary": summary,
                    }],
                    "summary": summary,
                }),
            )?;
        }
    }
    Ok(())
}

fn history_item_ids_for_call(
    connection: &Connection,
    kind: &str,
    call_id: &str,
) -> Result<Vec<String>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT id FROM protocol_history_items
         WHERE json_valid(payload_json)
           AND json_extract(payload_json, '$.kind') = ?1
           AND json_extract(payload_json, '$.call_id') = ?2
         ORDER BY id ASC",
    )?;
    let rows = statement
        .query_map((kind, call_id), |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn canonical_history_contains_change(
    connection: &Connection,
    change_id: &str,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM protocol_history_items, json_each(protocol_history_items.payload_json, '$.change_ids')
                 WHERE json_valid(protocol_history_items.payload_json)
                   AND json_extract(protocol_history_items.payload_json, '$.kind') = 'file_change'
                   AND json_each.value = ?1
             )",
            [change_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(StorageError::from)
}

fn canonical_ulid_or_new(value: &str) -> String {
    Ulid::from_string(value)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| new_migration_protocol_id())
}

fn rebuild_protocol_append_order(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch(
        "CREATE TEMP TABLE v33_append_order AS
         SELECT session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms
         FROM protocol_item_append_order
         ORDER BY created_at_ms ASC, sequence_no ASC, source_kind ASC, source_id ASC;
         DELETE FROM protocol_item_append_order;
         DELETE FROM sqlite_sequence WHERE name = 'protocol_item_append_order';
         INSERT INTO protocol_item_append_order
         (session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms)
         SELECT session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms
         FROM v33_append_order;
         DROP TABLE v33_append_order;",
    )?;
    Ok(())
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

fn validate_canonical_protocol_schema(connection: &Connection) -> Result<(), StorageError> {
    if !schema_migration_applied(connection, DURABLE_TURN_INPUT_QUEUE_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V51 durable turn-input queue marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        DURABLE_TURN_INPUT_QUEUE_VERSION,
        DURABLE_TURN_INPUT_QUEUE_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V51 durable turn-input queue marker has a name other than `{DURABLE_TURN_INPUT_QUEUE_NAME}`"
        )));
    }
    validate_durable_turn_input_queue_schema(connection)?;
    if !schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V50 durable agent-mailbox marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        DURABLE_AGENT_MAILBOX_VERSION,
        DURABLE_AGENT_MAILBOX_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V50 durable agent-mailbox marker has a name other than `{DURABLE_AGENT_MAILBOX_NAME}`"
        )));
    }
    validate_durable_agent_mailbox_schema(connection)?;
    if !schema_migration_applied(connection, AGENT_TREE_STOP_FENCES_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V49 agent tree-stop fence marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        AGENT_TREE_STOP_FENCES_VERSION,
        AGENT_TREE_STOP_FENCES_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V49 tree-stop fence marker has a name other than `{AGENT_TREE_STOP_FENCES_NAME}`"
        )));
    }
    if !schema_migration_applied(connection, AGENT_OWNER_RESUME_REQUESTS_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V48 agent owner-resume marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        AGENT_OWNER_RESUME_REQUESTS_VERSION,
        AGENT_OWNER_RESUME_REQUESTS_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V48 owner-resume marker has a name other than `{AGENT_OWNER_RESUME_REQUESTS_NAME}`"
        )));
    }
    validate_agent_owner_resume_request_schema(connection)?;
    if !schema_migration_applied(connection, RECURSIVE_SESSION_SPAWN_EDGES_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V47 recursive session spawn-edge marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        RECURSIVE_SESSION_SPAWN_EDGES_VERSION,
        RECURSIVE_SESSION_SPAWN_EDGES_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V47 recursive spawn-edge marker has a name other than `{RECURSIVE_SESSION_SPAWN_EDGES_NAME}`"
        )));
    }
    if !schema_migration_applied(connection, CODEX_COMPACTION_CHECKPOINT_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V46 Codex compaction checkpoint marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        CODEX_COMPACTION_CHECKPOINT_VERSION,
        CODEX_COMPACTION_CHECKPOINT_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V46 compaction checkpoint marker has a name other than `{CODEX_COMPACTION_CHECKPOINT_NAME}`"
        )));
    }
    if !schema_migration_applied(connection, RESTORE_AUTO_REVIEW_ACCESS_MODE_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V45 auto-review access mode restoration marker"
                .to_string(),
        ));
    }
    if !schema_migration_applied(connection, UNIQUE_TURN_TERMINAL_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V44 unique turn-terminal marker".to_string(),
        ));
    }
    if !schema_migration_applied(connection, INDEXED_INTERNAL_FILE_OWNERSHIP_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V43 indexed internal-file ownership marker".to_string(),
        ));
    }
    if !schema_migration_applied(connection, TYPED_HISTORY_SCOPE_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V42 typed history-scope marker".to_string(),
        ));
    }
    if !schema_migration_applied(connection, INDEXED_COLLABORATION_MODE_LOOKUP_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V41 indexed collaboration-mode lookup marker"
                .to_string(),
        ));
    }
    if !schema_migration_applied(connection, FLATTEN_SESSION_SPAWN_EDGES_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V40 flat session spawn-edge marker".to_string(),
        ));
    }
    if !schema_migration_applied(connection, TERMINAL_OUTCOME_CUTOVER_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V39 terminal outcome cutover marker".to_string(),
        ));
    }
    if !schema_migration_applied(connection, REMOVE_AUTO_REVIEW_ACCESS_MODE_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V38 auto-review access mode removal marker".to_string(),
        ));
    }
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
    if !table_has_exact_access_mode_domain(connection, "sessions", SESSION_ACCESS_MODE_DOMAIN)? {
        return Err(StorageError::Message(
            "V45 access mode marker exists but sessions does not have the exact default/auto_review/full_access domain"
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
    validate_typed_history_scope_schema(connection)?;
    validate_indexed_collaboration_mode_lookup(connection)?;
    validate_indexed_internal_file_ownership(connection)?;
    validate_unique_turn_terminal_index(connection)?;
    validate_recursive_session_spawn_edge_schema(connection)?;
    Ok(())
}

fn validate_canonical_protocol_storage(connection: &Connection) -> Result<(), StorageError> {
    validate_canonical_protocol_schema(connection)?;
    validate_agent_owner_resume_request_data(connection)?;
    validate_typed_history_scope_data(connection)?;
    if legacy_reasoning_projection_row_count(connection)? != 0 {
        return Err(StorageError::Message(
            "V36 legacy reasoning item removal marker exists but retired reasoning or prompt-dispatch protocol rows remain"
                .to_string(),
        ));
    }
    validate_recursive_session_spawn_edge_data(connection)?;
    validate_terminal_outcome_storage(connection)?;
    validate_raw_tool_call_history(connection)?;
    validate_compaction_checkpoint_history(connection)?;
    Ok(())
}

fn validate_typed_history_scope_schema(connection: &Connection) -> Result<(), StorageError> {
    let history_columns = connection
        .prepare("SELECT name, [notnull] FROM pragma_table_info('protocol_history_items')")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if !history_columns.contains(&("scope_kind".to_string(), 1))
        || !history_columns.contains(&("turn_id".to_string(), 0))
    {
        return Err(StorageError::Message(
            "V42 marker exists but protocol_history_items does not expose required scope_kind and nullable turn_id columns"
                .to_string(),
        ));
    }
    let append_columns = connection
        .prepare("SELECT name, [notnull] FROM pragma_table_info('protocol_item_append_order')")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if !append_columns.contains(&("scope_kind".to_string(), 1))
        || !append_columns.contains(&("turn_id".to_string(), 0))
    {
        return Err(StorageError::Message(
            "V42 marker exists but protocol_item_append_order does not expose required scope_kind and nullable turn_id columns"
                .to_string(),
        ));
    }

    let append_scope_fragment = if schema_migration_applied(
        connection,
        DURABLE_AGENT_MAILBOX_VERSION,
    )? {
        "scope_kind = 'session' and turn_id is null and source_kind in ('history_item', 'mailbox_message')"
    } else {
        "scope_kind = 'session' and turn_id is null and source_kind = 'history_item'"
    };
    for (table, required_fragments) in [
        (
            "protocol_history_items",
            &[
                "scope_kind in ('turn', 'session')",
                "scope_kind = 'turn' and turn_id is not null",
                "scope_kind = 'session' and turn_id is null",
            ][..],
        ),
        (
            "protocol_item_append_order",
            &[
                "scope_kind in ('turn', 'session')",
                "scope_kind = 'turn' and turn_id is not null",
                append_scope_fragment,
            ][..],
        ),
    ] {
        let sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "V42 marker exists but required table `{table}` is missing"
                ))
            })?;
        let normalized = sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        for fragment in required_fragments {
            if !normalized.contains(fragment) {
                return Err(StorageError::Message(format!(
                    "V42 marker exists but `{table}` lacks typed-scope constraint `{fragment}`"
                )));
            }
        }
    }

    for index_name in [
        "idx_protocol_history_turn_sequence",
        "idx_protocol_history_session_sequence",
    ] {
        let contract = connection
            .query_row(
                "SELECT [unique], origin, partial
                 FROM pragma_index_list('protocol_history_items')
                 WHERE name = ?1",
                [index_name],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        if contract != Some((1, "c".to_string(), 1)) {
            return Err(StorageError::Message(format!(
                "V42 marker exists but partial unique index `{index_name}` is missing or stale"
            )));
        }
    }
    Ok(())
}

fn validate_typed_history_scope_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_history_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM protocol_history_items
         WHERE (scope_kind = 'turn') <> (turn_id IS NOT NULL)
            OR (scope_kind = 'session' AND json_extract(payload_json, '$.kind') NOT IN (
                    'collaboration_mode_instruction', 'inter_agent_communication'
               ))
            OR (scope_kind = 'turn'
                AND json_extract(payload_json, '$.kind') = 'collaboration_mode_instruction')",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_history_rows != 0 {
        return Err(StorageError::Message(format!(
            "V42 typed history scope has {invalid_history_rows} invalid durable row(s)"
        )));
    }
    let mailbox_is_current = schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)?;
    let invalid_append_rows = if mailbox_is_current {
        connection.query_row(
            "SELECT COUNT(*)
             FROM protocol_item_append_order AS append_order
             LEFT JOIN protocol_history_items AS history
               ON append_order.source_kind = 'history_item'
              AND history.id = append_order.source_id
              AND history.session_id = append_order.session_id
             LEFT JOIN agent_mailbox_messages AS mailbox
               ON append_order.source_kind = 'mailbox_message'
              AND mailbox.id = append_order.source_id
              AND mailbox.recipient_session_id = append_order.session_id
             WHERE (append_order.scope_kind = 'turn') <>
                   (append_order.turn_id IS NOT NULL)
                OR (append_order.scope_kind = 'session'
                    AND append_order.source_kind NOT IN (
                        'history_item',
                        'mailbox_message'
                    ))
                OR (append_order.source_kind = 'mailbox_message'
                    AND (
                        mailbox.id IS NULL
                        OR append_order.scope_kind <> 'session'
                        OR append_order.turn_id IS NOT NULL
                    ))
                OR (append_order.source_kind = 'history_item'
                    AND (
                        history.id IS NULL
                        OR history.scope_kind <> append_order.scope_kind
                        OR history.turn_id IS NOT append_order.turn_id
                        OR history.sequence_no <> append_order.sequence_no
                    ))",
            [],
            |row| row.get::<_, i64>(0),
        )?
    } else {
        connection.query_row(
            "SELECT COUNT(*)
             FROM protocol_item_append_order AS append_order
             LEFT JOIN protocol_history_items AS history
               ON append_order.source_kind = 'history_item'
              AND history.id = append_order.source_id
              AND history.session_id = append_order.session_id
             WHERE (append_order.scope_kind = 'turn') <>
                   (append_order.turn_id IS NOT NULL)
                OR (append_order.scope_kind = 'session'
                    AND append_order.source_kind <> 'history_item')
                OR (append_order.source_kind = 'history_item'
                    AND (
                        history.id IS NULL
                        OR history.scope_kind <> append_order.scope_kind
                        OR history.turn_id IS NOT append_order.turn_id
                        OR history.sequence_no <> append_order.sequence_no
                    ))",
            [],
            |row| row.get::<_, i64>(0),
        )?
    };
    if invalid_append_rows != 0 {
        return Err(StorageError::Message(format!(
            "V42 typed append order has {invalid_append_rows} invalid scope projection row(s)"
        )));
    }
    Ok(())
}

fn validate_indexed_collaboration_mode_lookup(connection: &Connection) -> Result<(), StorageError> {
    const INDEX_NAME: &str = "idx_protocol_history_collaboration_mode_session";
    let index: Option<(i64, String, i64)> = connection
        .query_row(
            "SELECT [unique], origin, partial
             FROM pragma_index_list('protocol_history_items')
             WHERE name = ?1",
            [INDEX_NAME],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if index != Some((0, "c".to_string(), 1)) {
        return Err(StorageError::Message(format!(
            "V42 marker exists but partial index `{INDEX_NAME}` is missing or stale"
        )));
    }
    let mut statement = connection.prepare(
        "SELECT name
         FROM pragma_index_xinfo(?1)
         WHERE key = 1
         ORDER BY seqno ASC",
    )?;
    let columns = statement
        .query_map([INDEX_NAME], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns != ["session_id", "id"] {
        return Err(StorageError::Message(format!(
            "V42 marker exists but partial index `{INDEX_NAME}` has columns {columns:?} instead of [\"session_id\", \"id\"]"
        )));
    }
    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [INDEX_NAME],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "V42 marker exists but partial index `{INDEX_NAME}` has no schema definition"
            ))
        })?;
    let normalized = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let predicate = normalized
        .split_once(" where ")
        .map(|(_, predicate)| predicate.trim_end_matches(';'));
    let expected = "scope_kind = 'session' and json_extract(payload_json, '$.kind') = 'collaboration_mode_instruction'";
    if predicate != Some(expected) {
        return Err(StorageError::Message(format!(
            "V42 marker exists but partial index `{INDEX_NAME}` has a stale predicate"
        )));
    }
    Ok(())
}

fn validate_indexed_internal_file_ownership(connection: &Connection) -> Result<(), StorageError> {
    const INDEX_NAME: &str = "idx_tool_calls_truncated_output_path";
    let index: Option<(i64, String, i64)> = connection
        .query_row(
            "SELECT [unique], origin, partial
             FROM pragma_index_list('tool_calls')
             WHERE name = ?1",
            [INDEX_NAME],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if index != Some((0, "c".to_string(), 1)) {
        return Err(StorageError::Message(format!(
            "V43 marker exists but partial index `{INDEX_NAME}` is missing or stale"
        )));
    }

    let mut statement = connection.prepare(
        "SELECT name
         FROM pragma_index_xinfo(?1)
         WHERE key = 1
         ORDER BY seqno ASC",
    )?;
    let columns = statement
        .query_map([INDEX_NAME], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns != ["truncated_output_path"] {
        return Err(StorageError::Message(format!(
            "V43 marker exists but partial index `{INDEX_NAME}` has columns {columns:?} instead of [\"truncated_output_path\"]"
        )));
    }

    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [INDEX_NAME],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "V43 marker exists but partial index `{INDEX_NAME}` has no schema definition"
            ))
        })?;
    let normalized = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let predicate = normalized
        .split_once(" where ")
        .map(|(_, predicate)| predicate.trim_end_matches(';'));
    if predicate != Some("truncated_output_path is not null") {
        return Err(StorageError::Message(format!(
            "V43 marker exists but partial index `{INDEX_NAME}` has a stale predicate"
        )));
    }
    Ok(())
}

fn validate_unique_turn_terminal_index(connection: &Connection) -> Result<(), StorageError> {
    const INDEX_NAME: &str = "idx_protocol_runtime_events_unique_turn_terminal";
    let index: Option<(i64, String, i64)> = connection
        .query_row(
            "SELECT [unique], origin, partial
             FROM pragma_index_list('protocol_runtime_events')
             WHERE name = ?1",
            [INDEX_NAME],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if index != Some((1, "c".to_string(), 1)) {
        return Err(StorageError::Message(format!(
            "V44 marker exists but partial unique index `{INDEX_NAME}` is missing or stale"
        )));
    }

    let columns = connection
        .prepare(
            "SELECT name
             FROM pragma_index_xinfo(?1)
             WHERE key = 1
             ORDER BY seqno ASC",
        )?
        .query_map([INDEX_NAME], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns != ["session_id", "turn_id"] {
        return Err(StorageError::Message(format!(
            "V44 marker exists but partial unique index `{INDEX_NAME}` has columns {columns:?} instead of [\"session_id\", \"turn_id\"]"
        )));
    }

    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [INDEX_NAME],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(format!(
                "V44 marker exists but partial unique index `{INDEX_NAME}` has no schema definition"
            ))
        })?;
    let normalized = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let predicate = normalized
        .split_once(" where ")
        .map(|(_, predicate)| predicate.trim_end_matches(';'));
    if predicate != Some("json_extract(msg_json, '$.kind') = 'turn_terminal'") {
        return Err(StorageError::Message(format!(
            "V44 marker exists but partial unique index `{INDEX_NAME}` has a stale predicate"
        )));
    }
    Ok(())
}

fn validate_flat_session_spawn_edge_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM session_spawn_edges
         WHERE parent_session_id <> root_session_id
            OR child_session_id = root_session_id
            OR task_name = ''
            OR task_name = 'root'
            OR task_name GLOB '*[^a-z0-9_]*'
            OR agent_path <> ('/root/' || task_name)",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_rows != 0 {
        return Err(StorageError::Message(format!(
            "V40 marker exists but {invalid_rows} non-flat session spawn edge(s) remain"
        )));
    }
    let max_direct_children = connection.query_row(
        "SELECT COALESCE(MAX(child_count), 0)
         FROM (
             SELECT COUNT(*) AS child_count
             FROM session_spawn_edges
             GROUP BY root_session_id
         )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if max_direct_children > 255 {
        return Err(StorageError::Message(format!(
            "V40 marker exists but one agent tree retains {max_direct_children} direct children"
        )));
    }

    Ok(())
}

fn validate_recursive_session_spawn_edge_schema(
    connection: &Connection,
) -> Result<(), StorageError> {
    let table_names: &[&str] =
        if schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)? {
            &["session_spawn_edges"]
        } else {
            &["session_spawn_edges", "agent_completion_handoffs"]
        };
    for table_name in table_names {
        validate_v47_table_schema(connection, table_name)?;
    }
    Ok(())
}

fn validate_agent_owner_resume_request_schema(connection: &Connection) -> Result<(), StorageError> {
    let table_names: &[&str] =
        if schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)? {
            &[
                "agent_deferred_completions",
                "agent_tree_stop_fences",
                "effective_agent_deferred_completions",
            ]
        } else {
            &[
                "agent_owner_resume_requests",
                "agent_deferred_completions",
                "agent_tree_stop_fences",
                "effective_agent_deferred_completions",
            ]
        };
    for table_name in table_names {
        let expected = canonical_agent_owner_resume_request_schema(table_name)?;
        let observed = normalized_schema_objects_for_table(connection, table_name)?;
        for ((object_type, object_name), expected_sql) in &expected {
            let Some(observed_sql) = observed.get(&(object_type.clone(), object_name.clone()))
            else {
                return Err(StorageError::Message(format!(
                    "V49 marker exists but required {object_type} `{object_name}` is missing"
                )));
            };
            if observed_sql != expected_sql {
                return Err(StorageError::Message(format!(
                    "V49 marker exists but {object_type} `{object_name}` has stale SQL"
                )));
            }
        }
        for (object_type, object_name) in observed.keys() {
            if !expected.contains_key(&(object_type.clone(), object_name.clone())) {
                return Err(StorageError::Message(format!(
                    "V49 marker exists but unexpected {object_type} `{object_name}` owns `{table_name}`"
                )));
            }
        }
    }
    Ok(())
}

fn canonical_agent_owner_resume_request_schema(
    table_name: &str,
) -> Result<NormalizedSchemaObjects, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE sessions (
             id TEXT PRIMARY KEY NOT NULL,
             project_id TEXT NOT NULL,
             status TEXT NOT NULL,
             active_run_id TEXT
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );
         CREATE TABLE session_spawn_edges (
             root_session_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             child_session_id TEXT PRIMARY KEY NOT NULL,
             agent_path TEXT NOT NULL,
             task_name TEXT NOT NULL,
             spawn_order INTEGER NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_runtime_events (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             msg_json TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_history_items (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             scope_kind TEXT NOT NULL,
             payload_json TEXT NOT NULL
         );
         CREATE TABLE protocol_item_append_order (
             append_position INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL,
             scope_kind TEXT NOT NULL,
             turn_id TEXT,
             source_kind TEXT NOT NULL,
             source_id TEXT NOT NULL
         );
         CREATE TABLE agent_completion_handoffs (
             child_session_id TEXT NOT NULL,
             child_turn_id TEXT NOT NULL,
             child_terminal_event_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             parent_history_item_id TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL,
             PRIMARY KEY(child_session_id, child_turn_id)
         );",
    )?;
    connection.execute_batch(V48_AGENT_OWNER_RESUME_REQUESTS)?;
    connection.execute_batch(V49_AGENT_TREE_STOP_FENCES)?;
    normalized_schema_objects_for_table(&connection, table_name)
}

fn validate_durable_agent_mailbox_schema(connection: &Connection) -> Result<(), StorageError> {
    for table_name in [
        "agent_mailbox_messages",
        "protocol_item_append_order",
        "agent_completion_handoffs",
        "agent_owner_resume_requests",
    ] {
        let expected = canonical_durable_agent_mailbox_schema(table_name)?;
        let mut observed = normalized_schema_objects_for_table(connection, table_name)?;
        if table_name == "agent_mailbox_messages"
            && schema_migration_applied(connection, AGENT_TRIGGER_TURN_CLAIMS_VERSION)?
        {
            // V53 deliberately adds one trigger to the V50 mailbox table.  Its exact SQL is
            // owned and validated by validate_agent_trigger_turn_claims_schema below; remove
            // only that later-owned object before enforcing the otherwise closed V50 schema.
            observed.remove(&(
                "trigger".to_string(),
                "validate_claimed_agent_mailbox_resolution_before_update".to_string(),
            ));
        }
        for ((object_type, object_name), expected_sql) in &expected {
            let Some(observed_sql) = observed.get(&(object_type.clone(), object_name.clone()))
            else {
                return Err(StorageError::Message(format!(
                    "V50 marker exists but required {object_type} `{object_name}` is missing"
                )));
            };
            if observed_sql != expected_sql {
                return Err(StorageError::Message(format!(
                    "V50 marker exists but {object_type} `{object_name}` has stale SQL"
                )));
            }
        }
        for (object_type, object_name) in observed.keys() {
            if !expected.contains_key(&(object_type.clone(), object_name.clone())) {
                return Err(StorageError::Message(format!(
                    "V50 marker exists but unexpected {object_type} `{object_name}` owns `{table_name}`"
                )));
            }
        }
    }
    Ok(())
}

fn validate_durable_turn_input_queue_schema(connection: &Connection) -> Result<(), StorageError> {
    let expected_connection = canonical_durable_turn_input_queue_connection()?;
    for table_name in ["turn_steer_inputs", "turn_steer_input_enqueue_order"] {
        let expected = normalized_schema_objects_for_table(&expected_connection, table_name)?;
        let observed = normalized_schema_objects_for_table(connection, table_name)?;
        for ((object_type, object_name), expected_sql) in &expected {
            let Some(observed_sql) = observed.get(&(object_type.clone(), object_name.clone()))
            else {
                return Err(StorageError::Message(format!(
                    "V51 marker exists but required {object_type} `{object_name}` is missing"
                )));
            };
            if observed_sql != expected_sql {
                return Err(StorageError::Message(format!(
                    "V51 marker exists but {object_type} `{object_name}` has stale SQL"
                )));
            }
        }
        for (object_type, object_name) in observed.keys() {
            if !expected.contains_key(&(object_type.clone(), object_name.clone())) {
                return Err(StorageError::Message(format!(
                    "V51 marker exists but unexpected {object_type} `{object_name}` owns `{table_name}`"
                )));
            }
        }
    }
    Ok(())
}

fn validate_harness_turn_identity_schema(connection: &Connection) -> Result<(), StorageError> {
    if !schema_migration_applied(connection, HARNESS_TURN_IDENTITY_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V52 harness turn-identity marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        HARNESS_TURN_IDENTITY_VERSION,
        HARNESS_TURN_IDENTITY_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V52 harness turn-identity marker has a name other than `{HARNESS_TURN_IDENTITY_NAME}`"
        )));
    }

    let columns = connection
        .prepare(
            "SELECT name, type, [notnull]
             FROM pragma_table_info('harness_runs')",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, String>(1)?, row.get::<_, i64>(2)?),
            ))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    for column_name in ["protocol_turn_id", "canonical_terminal_runtime_event_id"] {
        let Some((declared_type, not_null)) = columns.get(column_name) else {
            return Err(StorageError::Message(format!(
                "V52 marker exists but harness_runs.{column_name} is missing"
            )));
        };
        if !declared_type.eq_ignore_ascii_case("TEXT") || *not_null != 0 {
            return Err(StorageError::Message(format!(
                "V52 marker exists but harness_runs.{column_name} is not nullable TEXT"
            )));
        }
    }

    let expected_connection = canonical_harness_turn_identity_connection()?;
    for (table_name, object_names) in [
        (
            "harness_runs",
            &[
                "idx_harness_runs_protocol_turn",
                "idx_harness_runs_canonical_terminal",
            ][..],
        ),
        (
            "harness_events",
            &["idx_harness_events_unique_run_terminal"][..],
        ),
    ] {
        let expected = normalized_schema_objects_for_table(&expected_connection, table_name)?;
        let observed = normalized_schema_objects_for_table(connection, table_name)?;
        for object_name in object_names {
            let key = ("index".to_string(), (*object_name).to_string());
            let Some(expected_sql) = expected.get(&key) else {
                return Err(StorageError::Message(format!(
                    "V52 canonical schema fixture is missing index `{object_name}`"
                )));
            };
            let Some(observed_sql) = observed.get(&key) else {
                return Err(StorageError::Message(format!(
                    "V52 marker exists but required index `{object_name}` is missing"
                )));
            };
            if observed_sql != expected_sql {
                return Err(StorageError::Message(format!(
                    "V52 marker exists but index `{object_name}` has stale SQL"
                )));
            }
        }
    }
    Ok(())
}

fn canonical_harness_turn_identity_connection() -> Result<Connection, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY);
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V14_HARNESS_ENGINE)?;
    connection.execute_batch(V52_HARNESS_TURN_IDENTITY)?;
    Ok(connection)
}

fn validate_harness_turn_identity_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_links = connection.query_row(
        "SELECT COUNT(*)
         FROM harness_runs AS run
         LEFT JOIN protocol_runtime_events AS terminal
           ON terminal.id = run.canonical_terminal_runtime_event_id
          AND terminal.session_id = run.session_id
          AND terminal.turn_id = run.protocol_turn_id
          AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
         LEFT JOIN protocol_item_append_order AS terminal_order
           ON terminal_order.session_id = terminal.session_id
          AND terminal_order.scope_kind = 'turn'
          AND terminal_order.turn_id = terminal.turn_id
          AND terminal_order.sequence_no = terminal.sequence_no
          AND terminal_order.source_kind = 'runtime_event'
          AND terminal_order.source_id = terminal.id
         WHERE (run.protocol_turn_id IS NOT NULL AND run.session_id IS NULL)
            OR (
                run.protocol_turn_id IS NOT NULL
                AND run.canonical_terminal_runtime_event_id IS NULL
                AND (
                    run.status <> '\"started\"'
                    OR run.completed_at_ms IS NOT NULL
                )
            )
            OR (
                run.canonical_terminal_runtime_event_id IS NOT NULL
                AND (
                    run.session_id IS NULL
                    OR run.protocol_turn_id IS NULL
                    OR run.completed_at_ms IS NULL
                    OR terminal.id IS NULL
                    OR terminal_order.append_position IS NULL
                    OR run.completed_at_ms <> terminal.created_at_ms
                    OR run.status <> CASE
                        json_extract(
                            terminal.msg_json,
                            '$.terminal.outcome.kind'
                        )
                        WHEN 'completed' THEN '\"pass\"'
                        WHEN 'interrupted' THEN '\"blocked\"'
                        WHEN 'failed' THEN '\"fail\"'
                        ELSE NULL
                    END
                    OR (
                        SELECT COUNT(*)
                        FROM harness_events AS harness_terminal
                        WHERE harness_terminal.run_id = run.id
                          AND json_extract(harness_terminal.kind, '$') =
                              'run_terminalized'
                    ) <> 1
                )
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_links != 0 {
        return Err(StorageError::Message(format!(
            "V52 marker exists but {invalid_links} harness run(s) violate canonical terminal linkage"
        )));
    }

    let mut statement = connection.prepare(
        "SELECT run.id, run.session_id, run.protocol_turn_id,
                run.canonical_terminal_runtime_event_id,
                terminal.msg_json, terminal.payload_sha256,
                harness_terminal.payload_json
         FROM harness_runs AS run
         INNER JOIN protocol_runtime_events AS terminal
           ON terminal.id = run.canonical_terminal_runtime_event_id
         INNER JOIN harness_events AS harness_terminal
           ON harness_terminal.run_id = run.id
          AND json_extract(harness_terminal.kind, '$') = 'run_terminalized'
         WHERE run.canonical_terminal_runtime_event_id IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    for row in rows {
        let (
            run_id,
            session_id,
            turn_id,
            terminal_event_id,
            terminal_msg_json,
            terminal_payload_sha256,
            harness_payload_json,
        ) = row?;
        turn_id
            .parse::<crate::protocol::TurnId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V52 marker exists but harness run {run_id} has invalid turn id `{turn_id}`: {error}"
                ))
            })?;
        terminal_event_id
            .parse::<crate::protocol::RuntimeEventId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V52 marker exists but harness run {run_id} has invalid terminal event id `{terminal_event_id}`: {error}"
                ))
            })?;
        if sha256_text(&terminal_msg_json) != terminal_payload_sha256 {
            return Err(StorageError::Message(format!(
                "V52 marker exists but harness run {run_id} links a terminal payload hash mismatch"
            )));
        }
        let msg =
            serde_json::from_str::<crate::protocol::RuntimeEventMsg>(&terminal_msg_json)
                .map_err(|error| {
                    StorageError::Message(format!(
                        "V52 marker exists but harness run {run_id} links an invalid typed terminal: {error}"
                    ))
                })?;
        let crate::protocol::RuntimeEventMsg::TurnTerminal { terminal } = msg else {
            return Err(StorageError::Message(format!(
                "V52 marker exists but harness run {run_id} does not link TurnTerminal"
            )));
        };
        let expected_payload = serde_json::to_string(
            &crate::harness::HarnessEventPayload::generic(
                serde_json::to_value(crate::session::RunEvent::TurnTerminal {
                    session_id: session_id
                    .parse::<crate::session::SessionId>()
                    .map_err(|error| {
                        StorageError::Message(format!(
                            "V52 marker exists but harness run {run_id} has invalid session id `{session_id}`: {error}"
                        ))
                    })?,
                    terminal,
                })?,
            ),
        )?;
        if harness_payload_json != expected_payload {
            return Err(StorageError::Message(format!(
                "V52 marker exists but harness run {run_id} terminal payload diverges from canonical truth"
            )));
        }
    }
    Ok(())
}

fn validate_agent_trigger_turn_claims_schema(connection: &Connection) -> Result<(), StorageError> {
    if !schema_migration_applied(connection, AGENT_TRIGGER_TURN_CLAIMS_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V53 agent trigger-turn claim marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        AGENT_TRIGGER_TURN_CLAIMS_VERSION,
        AGENT_TRIGGER_TURN_CLAIMS_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V53 agent trigger-turn claim marker has a name other than `{AGENT_TRIGGER_TURN_CLAIMS_NAME}`"
        )));
    }
    let expected_connection = canonical_agent_trigger_turn_claims_connection()?;
    let expected =
        normalized_schema_objects_for_table(&expected_connection, "agent_trigger_turn_claims")?;
    let observed = normalized_schema_objects_for_table(connection, "agent_trigger_turn_claims")?;
    for ((object_type, object_name), expected_sql) in &expected {
        let Some(observed_sql) = observed.get(&(object_type.clone(), object_name.clone())) else {
            return Err(StorageError::Message(format!(
                "V53 marker exists but required {object_type} `{object_name}` is missing"
            )));
        };
        if observed_sql != expected_sql {
            return Err(StorageError::Message(format!(
                "V53 marker exists but {object_type} `{object_name}` has stale SQL"
            )));
        }
    }
    for (object_type, object_name) in observed.keys() {
        if !expected.contains_key(&(object_type.clone(), object_name.clone())) {
            return Err(StorageError::Message(format!(
                "V53 marker exists but unexpected {object_type} `{object_name}` owns `agent_trigger_turn_claims`"
            )));
        }
    }
    let mailbox_trigger_key = (
        "trigger".to_string(),
        "validate_claimed_agent_mailbox_resolution_before_update".to_string(),
    );
    let expected_mailbox =
        normalized_schema_objects_for_table(&expected_connection, "agent_mailbox_messages")?;
    let observed_mailbox =
        normalized_schema_objects_for_table(connection, "agent_mailbox_messages")?;
    let expected_trigger = expected_mailbox.get(&mailbox_trigger_key).ok_or_else(|| {
        StorageError::Message(
            "canonical V53 schema omitted its claimed-mailbox resolution trigger".to_string(),
        )
    })?;
    let observed_trigger = observed_mailbox.get(&mailbox_trigger_key).ok_or_else(|| {
        StorageError::Message(
            "V53 marker exists but claimed-mailbox resolution trigger is missing".to_string(),
        )
    })?;
    if observed_trigger != expected_trigger {
        return Err(StorageError::Message(
            "V53 marker exists but claimed-mailbox resolution trigger has stale SQL".to_string(),
        ));
    }
    Ok(())
}

fn canonical_agent_trigger_turn_claims_connection() -> Result<Connection, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE sessions (
             id TEXT PRIMARY KEY,
             status TEXT NOT NULL,
             active_run_id TEXT,
             active_turn_id TEXT,
             active_run_lease_expires_at_ms INTEGER
         );
         CREATE TABLE agent_mailbox_messages (
              id TEXT PRIMARY KEY,
              recipient_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              trigger_turn INTEGER NOT NULL,
              state TEXT NOT NULL,
              delivered_turn_id TEXT,
              delivered_history_item_id TEXT,
              resolved_by_terminal_event_id TEXT,
              discarded_by_stopped_session_id TEXT,
              discarded_after_append_position INTEGER
          );
         CREATE TABLE protocol_runtime_events (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
             turn_id TEXT NOT NULL,
             msg_json TEXT NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V53_AGENT_TRIGGER_TURN_CLAIMS)?;
    Ok(connection)
}

fn validate_agent_trigger_turn_claims_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_claims = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_trigger_turn_claims AS claim
         LEFT JOIN sessions AS session
           ON session.id = claim.recipient_session_id
         LEFT JOIN agent_mailbox_messages AS mailbox
           ON mailbox.id = claim.history_item_id
           AND mailbox.recipient_session_id = claim.recipient_session_id
           AND mailbox.trigger_turn = 1
         WHERE mailbox.id IS NULL
            OR NOT (
                (
                    session.status = 'running'
                    AND session.active_run_id = claim.admission_id
                    AND session.active_turn_id = claim.turn_id
                    AND session.active_run_lease_expires_at_ms IS NOT NULL
                    AND (
                        mailbox.state = 'pending'
                        OR (
                            mailbox.state = 'delivered'
                            AND mailbox.delivered_turn_id = claim.turn_id
                            AND mailbox.delivered_history_item_id = mailbox.id
                            AND mailbox.resolved_by_terminal_event_id IS NULL
                            AND mailbox.discarded_by_stopped_session_id IS NULL
                            AND mailbox.discarded_after_append_position IS NULL
                        )
                        OR (
                            mailbox.state = 'discarded'
                            AND mailbox.resolved_by_terminal_event_id IS NULL
                            AND mailbox.discarded_by_stopped_session_id IS NOT NULL
                            AND mailbox.discarded_after_append_position IS NOT NULL
                        )
                    )
                )
                OR (
                    EXISTS (
                        SELECT 1
                        FROM protocol_runtime_events AS terminal
                        WHERE terminal.session_id = claim.recipient_session_id
                          AND terminal.turn_id = claim.turn_id
                          AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
                    )
                    AND (
                        (
                            mailbox.state = 'delivered'
                            AND mailbox.delivered_turn_id = claim.turn_id
                            AND mailbox.delivered_history_item_id = mailbox.id
                            AND mailbox.resolved_by_terminal_event_id IS NULL
                            AND mailbox.discarded_by_stopped_session_id IS NULL
                            AND mailbox.discarded_after_append_position IS NULL
                        )
                        OR (
                            mailbox.state = 'discarded'
                            AND mailbox.resolved_by_terminal_event_id IS NOT NULL
                            AND mailbox.discarded_by_stopped_session_id IS NULL
                            AND mailbox.discarded_after_append_position IS NULL
                            AND EXISTS (
                                SELECT 1
                                FROM protocol_runtime_events AS resolver
                                WHERE resolver.id = mailbox.resolved_by_terminal_event_id
                                  AND resolver.session_id = claim.recipient_session_id
                                  AND resolver.turn_id = claim.turn_id
                                  AND json_extract(resolver.msg_json, '$.kind')
                                      = 'turn_terminal'
                                  AND json_extract(
                                          resolver.msg_json,
                                          '$.terminal.outcome.kind'
                                      ) = 'interrupted'
                            )
                        )
                        OR (
                            mailbox.state = 'discarded'
                            AND mailbox.resolved_by_terminal_event_id IS NULL
                            AND mailbox.discarded_by_stopped_session_id IS NOT NULL
                            AND mailbox.discarded_after_append_position IS NOT NULL
                            AND EXISTS (
                                SELECT 1
                                FROM protocol_runtime_events AS stopped_terminal
                                WHERE stopped_terminal.session_id =
                                      claim.recipient_session_id
                                  AND stopped_terminal.turn_id = claim.turn_id
                                  AND json_extract(
                                          stopped_terminal.msg_json,
                                          '$.kind'
                                      ) = 'turn_terminal'
                                  AND json_extract(
                                          stopped_terminal.msg_json,
                                          '$.terminal.outcome.kind'
                                      ) = 'interrupted'
                                  AND json_extract(
                                          stopped_terminal.msg_json,
                                          '$.terminal.outcome.cause'
                                      ) = 'tree_stopped'
                            )
                        )
                    )
                )
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_claims != 0 {
        return Err(StorageError::Message(format!(
            "V53 marker exists but {invalid_claims} explicit agent wake claim(s) do not own an exact active or terminal turn"
        )));
    }
    Ok(())
}

fn canonical_durable_turn_input_queue_connection() -> Result<Connection, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE sessions (
             id TEXT PRIMARY KEY NOT NULL,
             project_id TEXT NOT NULL,
             status TEXT NOT NULL,
             active_run_id TEXT,
             active_turn_id TEXT,
             active_run_lease_expires_at_ms INTEGER
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );
         CREATE TABLE protocol_history_items (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             scope_kind TEXT NOT NULL,
             turn_id TEXT,
             sequence_no INTEGER NOT NULL,
             payload_json TEXT NOT NULL,
             payload_sha256 TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_runtime_events (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             sequence_no INTEGER NOT NULL,
             msg_json TEXT NOT NULL,
             payload_sha256 TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_item_append_order (
             append_position INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL,
             scope_kind TEXT NOT NULL,
             turn_id TEXT,
             sequence_no INTEGER NOT NULL,
             source_kind TEXT NOT NULL,
             source_id TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE agent_tree_stop_fences (
             stopped_session_id TEXT NOT NULL,
             after_append_position INTEGER NOT NULL,
             PRIMARY KEY(stopped_session_id, after_append_position)
         );",
    )?;
    connection.execute_batch(V51_DURABLE_TURN_INPUT_QUEUE)?;
    Ok(connection)
}

fn validate_durable_turn_input_queue_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_inputs = connection.query_row(
        "SELECT COUNT(*)
         FROM turn_steer_inputs AS input
         LEFT JOIN sessions AS session
           ON session.id = input.session_id
         LEFT JOIN turn_steer_input_enqueue_order AS enqueue
           ON enqueue.input_id = input.id
         LEFT JOIN protocol_history_items AS history
           ON history.id = input.id
         LEFT JOIN protocol_item_append_order AS history_order
           ON history_order.source_kind = 'history_item'
          AND history_order.source_id = input.id
         LEFT JOIN protocol_runtime_events AS terminal
           ON terminal.id = input.resolved_by_terminal_event_id
         LEFT JOIN protocol_item_append_order AS terminal_order
           ON terminal_order.source_kind = 'runtime_event'
          AND terminal_order.source_id = input.resolved_by_terminal_event_id
         WHERE session.id IS NULL
            OR json_extract(input.payload_json, '$.kind') IS NOT 'steer_turn'
            OR json_extract(input.payload_json, '$.expected_turn_id')
               IS NOT input.turn_id
            OR input.accepted_at_ms < 0
            OR input.updated_at_ms < input.accepted_at_ms
            OR (
                input.origin_kind = 'runtime'
                AND (
                    enqueue.input_id IS NULL
                    OR enqueue.session_id <> input.session_id
                    OR enqueue.turn_id <> input.turn_id
                    OR enqueue.created_at_ms <> input.accepted_at_ms
                )
            )
            OR (
                input.origin_kind IN ('legacy_history', 'fork')
                AND enqueue.input_id IS NOT NULL
            )
            OR (
                input.state = 'queued'
                AND (
                    input.origin_kind <> 'runtime'
                    OR session.status <> 'running'
                    OR session.active_run_id IS NOT input.admission_id
                    OR session.active_turn_id IS NOT input.turn_id
                    OR session.active_run_lease_expires_at_ms IS NULL
                    OR session.active_run_lease_expires_at_ms <
                       input.accepted_at_ms
                    OR history.id IS NOT NULL
                    OR history_order.append_position IS NOT NULL
                    OR input.delivered_at_ms IS NOT NULL
                    OR input.discarded_at_ms IS NOT NULL
                    OR input.resolved_by_terminal_event_id IS NOT NULL
                )
            )
            OR (
                input.state = 'delivered'
                AND (
                    history.id IS NULL
                    OR history.id <> input.id
                    OR history.session_id <> input.session_id
                    OR history.scope_kind <> 'turn'
                    OR history.turn_id IS NOT input.turn_id
                    OR history.payload_json <> input.payload_json
                    OR history.payload_sha256 <> input.payload_sha256
                    OR json_extract(history.payload_json, '$.kind')
                       IS NOT 'steer_turn'
                    OR history_order.append_position IS NULL
                    OR history_order.session_id <> input.session_id
                    OR history_order.scope_kind <> 'turn'
                    OR history_order.turn_id IS NOT input.turn_id
                    OR history_order.sequence_no <> history.sequence_no
                    OR input.delivered_at_ms IS NULL
                    OR input.delivered_at_ms < input.accepted_at_ms
                    OR input.updated_at_ms < input.delivered_at_ms
                    OR input.discarded_at_ms IS NOT NULL
                    OR input.resolved_by_terminal_event_id IS NOT NULL
                )
            )
            OR (
                input.state = 'discarded'
                AND (
                    input.origin_kind <> 'runtime'
                    OR history.id IS NOT NULL
                    OR history_order.append_position IS NOT NULL
                    OR input.delivered_at_ms IS NOT NULL
                    OR input.discarded_at_ms IS NULL
                    OR input.discarded_at_ms < input.accepted_at_ms
                    OR input.updated_at_ms < input.discarded_at_ms
                    OR terminal.id IS NULL
                    OR terminal.session_id <> input.session_id
                    OR terminal.turn_id <> input.turn_id
                    OR json_extract(terminal.msg_json, '$.kind')
                       IS NOT 'turn_terminal'
                    OR json_extract(
                           terminal.msg_json,
                           '$.terminal.outcome.kind'
                       ) IS NOT 'interrupted'
                    OR terminal.created_at_ms < input.accepted_at_ms
                    OR terminal_order.append_position IS NULL
                    OR terminal_order.session_id <> input.session_id
                    OR terminal_order.scope_kind <> 'turn'
                    OR terminal_order.turn_id IS NOT input.turn_id
                    OR terminal_order.sequence_no <> terminal.sequence_no
                )
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_inputs != 0 {
        return Err(StorageError::Message(format!(
            "V51 marker exists but {invalid_inputs} turn-steer input(s) violate queue, admission, order, or history identity"
        )));
    }

    {
        let mut statement = connection.prepare(
            "SELECT id, turn_id, payload_json, payload_sha256
                 FROM turn_steer_inputs",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (input_id, turn_id, payload_json, payload_sha256) = row?;
            if sha256_text(&payload_json) != payload_sha256 {
                return Err(StorageError::Message(format!(
                    "V51 marker exists but turn-steer input {input_id} has a payload hash mismatch"
                )));
            }
            let payload = serde_json::from_str::<crate::protocol::HistoryItemPayload>(
                &payload_json,
            )
            .map_err(|error| {
                StorageError::Message(format!(
                    "V51 marker exists but turn-steer input {input_id} has invalid typed payload: {error}"
                ))
            })?;
            let crate::protocol::HistoryItemPayload::SteerTurn {
                expected_turn_id, ..
            } = payload
            else {
                return Err(StorageError::Message(format!(
                    "V51 marker exists but turn-steer input {input_id} is not SteerTurn"
                )));
            };
            if expected_turn_id.to_string() != turn_id {
                return Err(StorageError::Message(format!(
                    "V51 marker exists but turn-steer input {input_id} targets {expected_turn_id}, not {turn_id}"
                )));
            }
        }
    }

    let unowned_orders = connection.query_row(
        "SELECT COUNT(*)
         FROM turn_steer_input_enqueue_order AS enqueue
         LEFT JOIN turn_steer_inputs AS input
           ON input.id = enqueue.input_id
         WHERE input.id IS NULL
            OR input.origin_kind <> 'runtime'
            OR input.session_id <> enqueue.session_id
            OR input.turn_id <> enqueue.turn_id",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if unowned_orders != 0 {
        return Err(StorageError::Message(format!(
            "V51 marker exists but {unowned_orders} turn-steer enqueue position(s) have no exact runtime owner"
        )));
    }

    let unowned_history = connection.query_row(
        "SELECT COUNT(*)
         FROM protocol_history_items AS history
         WHERE history.scope_kind = 'turn'
           AND json_extract(history.payload_json, '$.kind') = 'steer_turn'
           AND NOT EXISTS (
               SELECT 1
               FROM turn_steer_inputs AS input
               WHERE input.id = history.id
                 AND input.state = 'delivered'
                 AND input.delivered_history_item_id = history.id
           )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if unowned_history != 0 {
        return Err(StorageError::Message(format!(
            "V51 marker exists but {unowned_history} canonical SteerTurn item(s) have no delivered queue provenance"
        )));
    }
    Ok(())
}

fn canonical_durable_agent_mailbox_schema(
    table_name: &str,
) -> Result<NormalizedSchemaObjects, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE sessions (
             id TEXT PRIMARY KEY NOT NULL,
             project_id TEXT NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );
         CREATE TABLE session_spawn_edges (
             root_session_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             child_session_id TEXT PRIMARY KEY NOT NULL,
             agent_path TEXT NOT NULL
         );
         CREATE TABLE protocol_runtime_events (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             msg_json TEXT NOT NULL
         );
         CREATE TABLE protocol_history_items (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             scope_kind TEXT NOT NULL,
             turn_id TEXT,
             sequence_no INTEGER NOT NULL,
             payload_json TEXT NOT NULL,
             payload_sha256 TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_item_append_order (
             append_position INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL,
             scope_kind TEXT NOT NULL,
             turn_id TEXT,
             sequence_no INTEGER NOT NULL,
             source_kind TEXT NOT NULL,
             source_id TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE INDEX idx_protocol_item_append_order_session_position
             ON protocol_item_append_order(session_id, append_position ASC);
         CREATE INDEX idx_protocol_item_append_order_turn_position
             ON protocol_item_append_order(
                 session_id,
                 turn_id,
                 append_position ASC
             )
             WHERE scope_kind = 'turn';
         CREATE TABLE agent_tree_stop_fences (
             root_session_id TEXT NOT NULL,
             stopped_session_id TEXT NOT NULL,
             after_append_position INTEGER NOT NULL,
             cause TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL,
             PRIMARY KEY(stopped_session_id, after_append_position)
         );
         CREATE TABLE agent_completion_handoffs (
             child_session_id TEXT NOT NULL,
             child_turn_id TEXT NOT NULL,
             child_terminal_event_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             parent_history_item_id TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL,
             PRIMARY KEY(child_session_id, child_turn_id)
         );
         CREATE TABLE agent_owner_resume_requests (
             owner_session_id TEXT NOT NULL,
             source_session_id TEXT NOT NULL,
             source_history_item_id TEXT NOT NULL,
             state TEXT NOT NULL,
             claimed_turn_id TEXT,
             created_at_ms INTEGER NOT NULL,
             updated_at_ms INTEGER NOT NULL,
             claimed_at_ms INTEGER,
             resolved_at_ms INTEGER,
             PRIMARY KEY(owner_session_id, source_history_item_id)
         );",
    )?;
    connection.execute_batch(V50_DURABLE_AGENT_MAILBOX)?;
    normalized_schema_objects_for_table(&connection, table_name)
}

fn validate_durable_agent_mailbox_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_identity = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_mailbox_messages AS mailbox
         LEFT JOIN sessions AS root ON root.id = mailbox.root_session_id
         LEFT JOIN sessions AS author ON author.id = mailbox.author_session_id
         LEFT JOIN sessions AS recipient
           ON recipient.id = mailbox.recipient_session_id
         LEFT JOIN session_spawn_edges AS author_edge
           ON author_edge.root_session_id = mailbox.root_session_id
          AND author_edge.child_session_id = mailbox.author_session_id
         LEFT JOIN session_spawn_edges AS recipient_edge
           ON recipient_edge.root_session_id = mailbox.root_session_id
          AND recipient_edge.child_session_id = mailbox.recipient_session_id
         WHERE root.id IS NULL
            OR recipient.id IS NULL
            OR root.project_id <> recipient.project_id
            OR (
                mailbox.author_session_id IS NULL
                AND mailbox.state = 'pending'
            )
            OR (
                mailbox.author_session_id IS NOT NULL
                AND (
                    author.id IS NULL
                    OR root.project_id <> author.project_id
                )
            )
            OR EXISTS (
                SELECT 1
                FROM session_spawn_edges AS root_edge
                WHERE root_edge.child_session_id = mailbox.root_session_id
            )
            OR json_extract(mailbox.payload_json, '$.kind')
               IS NOT 'inter_agent_communication'
            OR json_extract(
                   mailbox.payload_json,
                   '$.communication.trigger_turn'
               ) IS NOT mailbox.trigger_turn
            OR (
                mailbox.author_session_id IS NOT NULL
                AND
                mailbox.author_session_id = mailbox.root_session_id
                AND json_extract(
                        mailbox.payload_json,
                        '$.communication.author'
                    ) IS NOT '/root'
            )
            OR (
                mailbox.author_session_id IS NOT NULL
                AND
                mailbox.author_session_id <> mailbox.root_session_id
                AND (
                    author_edge.child_session_id IS NULL
                    OR json_extract(
                           mailbox.payload_json,
                           '$.communication.author'
                       ) IS NOT author_edge.agent_path
                )
            )
            OR (
                mailbox.recipient_session_id = mailbox.root_session_id
                AND json_extract(
                        mailbox.payload_json,
                        '$.communication.recipient'
                    ) IS NOT '/root'
            )
            OR (
                mailbox.recipient_session_id <> mailbox.root_session_id
                AND (
                    recipient_edge.child_session_id IS NULL
                    OR json_extract(
                           mailbox.payload_json,
                           '$.communication.recipient'
                       ) IS NOT recipient_edge.agent_path
                )
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_identity != 0 {
        return Err(StorageError::Message(format!(
            "V50 marker exists but {invalid_identity} mailbox message(s) violate canonical tree or payload identity"
        )));
    }

    let invalid_lifecycle = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_mailbox_messages AS mailbox
         LEFT JOIN protocol_history_items AS history
           ON history.id = mailbox.delivered_history_item_id
          AND history.session_id = mailbox.recipient_session_id
         WHERE (
                mailbox.state = 'pending'
                AND (
                    history.id IS NOT NULL
                    OR NOT EXISTS (
                        SELECT 1
                        FROM protocol_item_append_order AS enqueue_order
                        WHERE enqueue_order.session_id =
                              mailbox.recipient_session_id
                          AND enqueue_order.scope_kind = 'session'
                          AND enqueue_order.turn_id IS NULL
                          AND enqueue_order.source_kind = 'mailbox_message'
                          AND enqueue_order.source_id = mailbox.id
                    )
                )
            )
            OR (
                mailbox.state = 'discarded'
                AND (
                    history.id IS NOT NULL
                    OR NOT EXISTS (
                        SELECT 1
                        FROM protocol_item_append_order AS enqueue_order
                        WHERE enqueue_order.session_id =
                              mailbox.recipient_session_id
                          AND enqueue_order.scope_kind = 'session'
                          AND enqueue_order.turn_id IS NULL
                          AND enqueue_order.source_kind = 'mailbox_message'
                          AND enqueue_order.source_id = mailbox.id
                    )
                )
            )
            OR (
                mailbox.state = 'delivered'
                AND (
                    history.id IS NULL
                    OR history.payload_json <> mailbox.payload_json
                    OR history.payload_sha256 <> mailbox.payload_sha256
                    OR (
                        history.scope_kind = 'turn'
                        AND history.turn_id IS NOT mailbox.delivered_turn_id
                    )
                    OR (
                        history.scope_kind = 'session'
                        AND mailbox.delivered_turn_id IS NOT NULL
                    )
                )
            )
            OR (
                mailbox.state <> 'delivered'
                AND EXISTS (
                    SELECT 1
                    FROM protocol_history_items AS unexpected_history
                    WHERE unexpected_history.id = mailbox.id
                )
            )
            OR (
                mailbox.state = 'delivered'
                AND NOT EXISTS (
                    SELECT 1
                    FROM protocol_item_append_order AS history_order
                    WHERE history_order.session_id =
                          mailbox.recipient_session_id
                      AND history_order.source_kind = 'history_item'
                      AND history_order.source_id = mailbox.id
                )
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_lifecycle != 0 {
        return Err(StorageError::Message(format!(
            "V50 marker exists but {invalid_lifecycle} mailbox message(s) violate pending, delivered, or discarded projection state"
        )));
    }

    let orphan_history = connection.query_row(
        "SELECT COUNT(*)
         FROM protocol_history_items AS history
         LEFT JOIN agent_mailbox_messages AS mailbox ON mailbox.id = history.id
         LEFT JOIN session_spawn_edges AS recipient_edge
           ON recipient_edge.child_session_id = history.session_id
         WHERE json_extract(history.payload_json, '$.kind')
               = 'inter_agent_communication'
           AND json_extract(
                   history.payload_json,
                   '$.communication.recipient'
               ) = CASE
                       WHEN recipient_edge.child_session_id IS NULL
                       THEN '/root'
                       ELSE recipient_edge.agent_path
                   END
           AND (
               json_extract(
                   history.payload_json,
                   '$.communication.author'
               ) = '/root'
               OR EXISTS (
                   SELECT 1
                   FROM session_spawn_edges AS author_edge
                   WHERE author_edge.root_session_id =
                         COALESCE(
                             recipient_edge.root_session_id,
                             history.session_id
                         )
                     AND author_edge.agent_path = json_extract(
                         history.payload_json,
                         '$.communication.author'
                     )
               )
           )
           AND (
               mailbox.id IS NULL
               OR mailbox.state <> 'delivered'
               OR mailbox.recipient_session_id <> history.session_id
           )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if orphan_history != 0 {
        return Err(StorageError::Message(format!(
            "V50 marker exists but {orphan_history} delivered IAC history item(s) have no durable delivered mailbox owner"
        )));
    }

    let mut statement = connection.prepare(
        "SELECT id, payload_json, payload_sha256
         FROM agent_mailbox_messages
         ORDER BY id ASC",
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
        let mut digest = Sha256::new();
        digest.update(payload_json.as_bytes());
        let expected = format!("{:x}", digest.finalize());
        if payload_sha256 != expected {
            return Err(StorageError::Message(format!(
                "V50 mailbox message {id} has a stale payload hash"
            )));
        }
    }

    validate_durable_agent_mailbox_links(connection)
}

fn validate_durable_agent_mailbox_links(connection: &Connection) -> Result<(), StorageError> {
    let invalid_handoffs = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_completion_handoffs AS handoff
         LEFT JOIN session_spawn_edges AS edge
           ON edge.child_session_id = handoff.child_session_id
          AND edge.parent_session_id = handoff.parent_session_id
         LEFT JOIN protocol_runtime_events AS terminal
           ON terminal.id = handoff.child_terminal_event_id
          AND terminal.session_id = handoff.child_session_id
          AND terminal.turn_id = handoff.child_turn_id
         LEFT JOIN agent_mailbox_messages AS mailbox
           ON mailbox.id = handoff.parent_history_item_id
          AND mailbox.recipient_session_id = handoff.parent_session_id
          AND mailbox.author_session_id = handoff.child_session_id
         WHERE edge.child_session_id IS NULL
            OR terminal.id IS NULL
            OR json_extract(terminal.msg_json, '$.kind') IS NOT 'turn_terminal'
            OR COALESCE(
                   json_extract(terminal.msg_json, '$.terminal.outcome.kind'),
                   ''
               ) NOT IN ('completed', 'failed')
            OR mailbox.id IS NULL
            OR mailbox.trigger_turn <> 0
            OR instr(
                   COALESCE(
                       json_extract(
                           mailbox.payload_json,
                           '$.communication.content'
                       ),
                       ''
                   ),
                   'Message Type: FINAL_ANSWER' || char(10)
                         || 'Task name: ' ||
                         CASE
                             WHEN edge.parent_session_id =
                                  edge.root_session_id
                             THEN '/root'
                             ELSE (
                                 SELECT parent_edge.agent_path
                                 FROM session_spawn_edges AS parent_edge
                                 WHERE parent_edge.child_session_id =
                                       edge.parent_session_id
                             )
                         END
                         || char(10) || 'Sender: ' || edge.agent_path
                         || char(10) || 'Payload:' || char(10)
               ) <> 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_handoffs != 0 {
        return Err(StorageError::Message(format!(
            "V50 marker exists but {invalid_handoffs} completion handoff(s) do not reference their stable FINAL mailbox identity"
        )));
    }

    let invalid_resume = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_owner_resume_requests AS request
         LEFT JOIN sessions AS owner ON owner.id = request.owner_session_id
         LEFT JOIN sessions AS source ON source.id = request.source_session_id
         LEFT JOIN agent_mailbox_messages AS mailbox
           ON mailbox.id = request.source_history_item_id
          AND mailbox.recipient_session_id = request.source_session_id
         LEFT JOIN session_spawn_edges AS owner_edge
           ON owner_edge.child_session_id = request.owner_session_id
         WHERE owner.id IS NULL
            OR source.id IS NULL
            OR mailbox.id IS NULL
            OR owner_edge.child_session_id IS NULL
            OR request.source_session_id <> request.owner_session_id
            OR mailbox.trigger_turn <> 0
            OR request.state NOT IN ('pending', 'claimed', 'resolved', 'cancelled')
            OR (request.state = 'pending'
                AND (request.claimed_turn_id IS NOT NULL
                     OR request.claimed_at_ms IS NOT NULL
                     OR request.resolved_at_ms IS NOT NULL))
            OR (request.state = 'claimed'
                AND (request.claimed_turn_id IS NULL
                     OR request.claimed_at_ms IS NULL
                     OR request.resolved_at_ms IS NOT NULL))
            OR (request.state = 'resolved'
                AND (request.claimed_turn_id IS NULL
                     OR request.claimed_at_ms IS NULL
                     OR request.resolved_at_ms IS NULL))
            OR (request.state = 'cancelled'
                AND (request.resolved_at_ms IS NULL
                     OR (request.claimed_turn_id IS NULL)
                        IS NOT (request.claimed_at_ms IS NULL)))
            OR (
                (
                    request.state IN ('claimed', 'resolved')
                    OR (
                        request.state = 'cancelled'
                        AND request.claimed_turn_id IS NOT NULL
                    )
                )
                AND NOT EXISTS (
                    SELECT 1
                    FROM protocol_runtime_events AS started
                    WHERE started.session_id = request.owner_session_id
                      AND started.turn_id = request.claimed_turn_id
                      AND json_extract(started.msg_json, '$.kind')
                          = 'warning'
                      AND json_extract(started.msg_json, '$.message')
                          LIKE 'thread started:%'
                )
            )
            OR NOT EXISTS (
                SELECT 1
                FROM agent_completion_handoffs AS handoff
                INNER JOIN session_spawn_edges AS child_edge
                  ON child_edge.child_session_id = handoff.child_session_id
                 AND child_edge.parent_session_id =
                     request.owner_session_id
                WHERE handoff.parent_session_id =
                      request.owner_session_id
                  AND handoff.parent_history_item_id =
                      request.source_history_item_id
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_resume != 0 {
        return Err(StorageError::Message(format!(
            "V50 marker exists but {invalid_resume} owner-resume request(s) do not reference a direct-child stable mailbox identity"
        )));
    }
    Ok(())
}

fn validate_agent_owner_resume_request_data(connection: &Connection) -> Result<(), StorageError> {
    let mailbox_is_current = schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)?;
    if mailbox_is_current {
        validate_durable_agent_mailbox_links(connection)?;
    }
    let invalid_fences = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_tree_stop_fences AS fence
         LEFT JOIN sessions AS root
           ON root.id = fence.root_session_id
         LEFT JOIN sessions AS stopped
           ON stopped.id = fence.stopped_session_id
         WHERE root.id IS NULL
            OR stopped.id IS NULL
            OR stopped.project_id <> root.project_id
            OR fence.after_append_position < 0
            OR fence.after_append_position > MAX(
                COALESCE(
                    (
                        SELECT MAX(append_position)
                        FROM protocol_item_append_order
                    ),
                    0
                ),
                COALESCE(
                    (
                        SELECT seq
                        FROM sqlite_sequence
                        WHERE name = 'protocol_item_append_order'
                    ),
                    0
                )
            )
            OR fence.created_at_ms < 0
            OR fence.cause NOT IN (
                'approval_aborted',
                'user_stop',
                'tree_stopped',
                'root_failed'
            )
            OR (
                fence.cause = 'root_failed'
                AND fence.stopped_session_id <> fence.root_session_id
            )
            OR (
                fence.cause = 'tree_stopped'
                AND (
                    WITH RECURSIVE origin_scope(
                        root_session_id,
                        stopped_session_id,
                        after_append_position,
                        session_id
                    ) AS (
                        SELECT
                            origin.root_session_id,
                            origin.stopped_session_id,
                            origin.after_append_position,
                            origin.stopped_session_id
                        FROM agent_tree_stop_fences AS origin
                        WHERE origin.root_session_id =
                              fence.root_session_id
                          AND origin.after_append_position =
                              fence.after_append_position
                          AND origin.cause IN (
                              'approval_aborted',
                              'user_stop',
                              'root_failed'
                          )
                        UNION ALL
                        SELECT
                            origin_scope.root_session_id,
                            origin_scope.stopped_session_id,
                            origin_scope.after_append_position,
                            child.child_session_id
                        FROM origin_scope
                        INNER JOIN session_spawn_edges AS child
                          ON child.root_session_id =
                             origin_scope.root_session_id
                         AND child.parent_session_id =
                             origin_scope.session_id
                    )
                    SELECT COUNT(*)
                    FROM origin_scope
                    WHERE session_id = fence.stopped_session_id
                ) <> 1
            )
            OR EXISTS (
                SELECT 1
                FROM session_spawn_edges AS owner
                WHERE owner.child_session_id = fence.root_session_id
            )
            OR (
                fence.stopped_session_id <> fence.root_session_id
                AND NOT EXISTS (
                    SELECT 1
                    FROM session_spawn_edges AS stopped_edge
                    WHERE stopped_edge.root_session_id =
                          fence.root_session_id
                      AND stopped_edge.child_session_id =
                          fence.stopped_session_id
                )
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_fences != 0 {
        return Err(StorageError::Message(format!(
            "V49 marker exists but {invalid_fences} tree-stop fence row(s) violate durable root, subtree, or append-boundary identity"
        )));
    }

    let invalid = if mailbox_is_current {
        0
    } else {
        connection.query_row(
            "SELECT COUNT(*)
         FROM agent_owner_resume_requests AS request
         LEFT JOIN sessions AS owner ON owner.id = request.owner_session_id
         LEFT JOIN sessions AS source ON source.id = request.source_session_id
         LEFT JOIN protocol_history_items AS history
           ON history.id = request.source_history_item_id
          AND history.session_id = request.source_session_id
         LEFT JOIN session_spawn_edges AS owner_edge
           ON owner_edge.child_session_id = request.owner_session_id
         WHERE owner.id IS NULL
            OR source.id IS NULL
            OR history.id IS NULL
            OR owner_edge.child_session_id IS NULL
            OR request.source_session_id <> request.owner_session_id
            OR json_extract(history.payload_json, '$.kind')
                 IS NOT 'inter_agent_communication'
            OR history.scope_kind <> 'session'
            OR json_extract(
                 history.payload_json,
                 '$.communication.trigger_turn'
               ) IS NOT 0
            OR NOT EXISTS (
                SELECT 1
                FROM agent_completion_handoffs AS handoff
                INNER JOIN session_spawn_edges AS child_edge
                  ON child_edge.child_session_id = handoff.child_session_id
                 AND child_edge.parent_session_id = request.owner_session_id
                WHERE handoff.parent_session_id = request.owner_session_id
                  AND handoff.parent_history_item_id =
                      request.source_history_item_id
            )
            OR request.state NOT IN ('pending', 'claimed', 'resolved', 'cancelled')
            OR (request.state = 'pending'
                AND (request.claimed_turn_id IS NOT NULL
                     OR request.claimed_at_ms IS NOT NULL
                     OR request.resolved_at_ms IS NOT NULL))
            OR (request.state = 'claimed'
                AND (request.claimed_turn_id IS NULL
                     OR request.claimed_at_ms IS NULL
                     OR request.resolved_at_ms IS NOT NULL))
            OR (request.state = 'resolved'
                AND (request.claimed_turn_id IS NULL
                     OR request.claimed_at_ms IS NULL
                     OR request.resolved_at_ms IS NULL))
            OR (request.state = 'cancelled'
                AND (request.resolved_at_ms IS NULL
                     OR (request.claimed_turn_id IS NULL)
                        IS NOT (request.claimed_at_ms IS NULL)))
            OR (
                (
                    request.state IN ('claimed', 'resolved')
                    OR (
                        request.state = 'cancelled'
                        AND request.claimed_turn_id IS NOT NULL
                    )
                )
                AND NOT EXISTS (
                    SELECT 1
                    FROM protocol_runtime_events AS started
                    WHERE started.session_id = request.owner_session_id
                      AND started.turn_id = request.claimed_turn_id
                      AND json_extract(started.msg_json, '$.kind')
                          = 'warning'
                      AND json_extract(started.msg_json, '$.message')
                          LIKE 'thread started:%'
                )
            )",
            [],
            |row| row.get::<_, i64>(0),
        )?
    };
    if invalid != 0 {
        return Err(StorageError::Message(format!(
            "V49 marker exists but {invalid} owner-resume request row(s) violate durable identity or state"
        )));
    }

    let invalid_deferred = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_deferred_completions AS deferred
         LEFT JOIN session_spawn_edges AS edge
           ON edge.child_session_id = deferred.agent_session_id
          AND edge.parent_session_id = deferred.parent_session_id
         LEFT JOIN protocol_runtime_events AS terminal
           ON terminal.id = deferred.terminal_event_id
          AND terminal.session_id = deferred.agent_session_id
          AND terminal.turn_id = deferred.agent_turn_id
         LEFT JOIN protocol_runtime_events AS resolver
           ON resolver.id = deferred.resolved_by_terminal_event_id
         WHERE edge.child_session_id IS NULL
            OR terminal.id IS NULL
            OR json_extract(terminal.msg_json, '$.kind') IS NOT 'turn_terminal'
            OR (
                deferred.kind = 'completed_early'
                AND json_extract(
                      terminal.msg_json,
                      '$.terminal.outcome.kind'
                    ) IS NOT 'completed'
            )
            OR (
                deferred.kind = 'crash_failed'
                AND json_extract(
                      terminal.msg_json,
                      '$.terminal.outcome.kind'
                    ) IS NOT 'failed'
            )
            OR deferred.kind NOT IN ('completed_early', 'crash_failed')
            OR deferred.state NOT IN ('pending', 'superseded', 'released', 'discarded')
            OR (
                deferred.state = 'pending'
                AND (
                    deferred.resolved_by_terminal_event_id IS NOT NULL
                    OR deferred.resolved_at_ms IS NOT NULL
                    OR EXISTS (
                        SELECT 1
                        FROM agent_completion_handoffs AS handoff
                        WHERE handoff.child_session_id = deferred.agent_session_id
                          AND handoff.child_turn_id = deferred.agent_turn_id
                    )
                )
            )
            OR (
                deferred.state IN ('superseded', 'released', 'discarded')
                AND (
                    resolver.id IS NULL
                    OR deferred.resolved_at_ms IS NULL
                )
            )
            OR (
                deferred.state = 'superseded'
                AND NOT (
                    EXISTS (
                        SELECT 1
                        FROM protocol_runtime_events AS source_terminal
                        INNER JOIN session_spawn_edges AS child_edge
                          ON child_edge.child_session_id =
                             source_terminal.session_id
                         AND child_edge.parent_session_id =
                             deferred.agent_session_id
                        INNER JOIN agent_completion_handoffs AS handoff
                          ON handoff.child_session_id =
                             source_terminal.session_id
                         AND handoff.child_turn_id = source_terminal.turn_id
                         AND handoff.parent_session_id =
                             deferred.agent_session_id
                        WHERE source_terminal.id =
                              deferred.resolved_by_terminal_event_id
                          AND json_extract(
                                source_terminal.msg_json,
                                '$.kind'
                              ) = 'turn_terminal'
                          AND json_extract(
                                source_terminal.msg_json,
                                '$.terminal.outcome.kind'
                              ) IN ('completed', 'failed')
                    )
                    OR (
                        deferred.kind = 'crash_failed'
                        AND EXISTS (
                            SELECT 1
                            FROM protocol_runtime_events AS source_terminal
                            INNER JOIN protocol_item_append_order AS source_order
                              ON source_order.session_id =
                                 source_terminal.session_id
                             AND source_order.source_kind = 'runtime_event'
                             AND source_order.source_id = source_terminal.id
                            INNER JOIN protocol_item_append_order AS deferred_order
                              ON deferred_order.session_id = terminal.session_id
                             AND deferred_order.source_kind = 'runtime_event'
                             AND deferred_order.source_id = terminal.id
                            WHERE source_terminal.id =
                                  deferred.resolved_by_terminal_event_id
                              AND source_terminal.session_id =
                                  deferred.agent_session_id
                              AND source_terminal.turn_id <>
                                  deferred.agent_turn_id
                              AND json_extract(
                                    source_terminal.msg_json,
                                    '$.kind'
                                  ) = 'turn_terminal'
                              AND json_extract(
                                    source_terminal.msg_json,
                                    '$.terminal.outcome.kind'
                                  ) IN ('completed', 'failed')
                              AND source_order.append_position >
                                  deferred_order.append_position
                        )
                    )
                )
            )
            OR (
                deferred.state = 'released'
                AND NOT EXISTS (
                    SELECT 1
                    FROM agent_completion_handoffs AS handoff
                    WHERE handoff.child_session_id = deferred.agent_session_id
                      AND handoff.child_turn_id = deferred.agent_turn_id
                )
            )
            OR (
                deferred.state = 'released'
                AND NOT EXISTS (
                    WITH RECURSIVE descendants(session_id) AS (
                        SELECT child.child_session_id
                        FROM session_spawn_edges AS child
                        WHERE child.parent_session_id =
                              deferred.agent_session_id
                        UNION ALL
                        SELECT child.child_session_id
                        FROM session_spawn_edges AS child
                        INNER JOIN descendants AS parent
                          ON child.parent_session_id = parent.session_id
                    )
                    SELECT 1
                    FROM descendants
                    INNER JOIN protocol_runtime_events AS resolution_terminal
                      ON resolution_terminal.session_id =
                         descendants.session_id
                     AND resolution_terminal.id =
                         deferred.resolved_by_terminal_event_id
                    INNER JOIN protocol_item_append_order AS resolution_order
                      ON resolution_order.session_id =
                         resolution_terminal.session_id
                     AND resolution_order.source_kind = 'runtime_event'
                     AND resolution_order.source_id =
                         resolution_terminal.id
                    INNER JOIN protocol_item_append_order AS deferred_order
                      ON deferred_order.session_id = terminal.session_id
                     AND deferred_order.source_kind = 'runtime_event'
                     AND deferred_order.source_id = terminal.id
                    WHERE json_extract(
                              resolution_terminal.msg_json,
                              '$.kind'
                          ) = 'turn_terminal'
                      AND json_extract(
                              resolution_terminal.msg_json,
                              '$.terminal.outcome.kind'
                          ) = 'interrupted'
                      AND (
                          (
                              deferred.state = 'released'
                              AND json_extract(
                                    resolution_terminal.msg_json,
                                    '$.terminal.outcome.cause'
                                  ) = 'agent_interrupted'
                          )
                          OR
                          (
                              deferred.state = 'discarded'
                              AND json_extract(
                                    resolution_terminal.msg_json,
                                    '$.terminal.outcome.cause'
                                  ) IN (
                                      'approval_aborted',
                                      'user_stop',
                                      'tree_stopped'
                                  )
                          )
                      )
                      AND resolution_order.append_position >
                          deferred_order.append_position
                )
            )
            OR (
                deferred.state = 'discarded'
                AND EXISTS (
                    SELECT 1
                    FROM agent_completion_handoffs AS handoff
                    WHERE handoff.child_session_id =
                          deferred.agent_session_id
                      AND handoff.child_turn_id = deferred.agent_turn_id
                )
            )
            OR (
                deferred.state = 'discarded'
                AND NOT (
                    EXISTS (
                        WITH RECURSIVE descendants(session_id) AS (
                            SELECT child.child_session_id
                            FROM session_spawn_edges AS child
                            WHERE child.parent_session_id =
                                  deferred.agent_session_id
                            UNION ALL
                            SELECT child.child_session_id
                            FROM session_spawn_edges AS child
                            INNER JOIN descendants AS parent
                              ON child.parent_session_id = parent.session_id
                        )
                        SELECT 1
                        FROM descendants
                        INNER JOIN protocol_runtime_events AS resolution_terminal
                          ON resolution_terminal.session_id =
                             descendants.session_id
                         AND resolution_terminal.id =
                             deferred.resolved_by_terminal_event_id
                        INNER JOIN protocol_item_append_order AS resolution_order
                          ON resolution_order.session_id =
                             resolution_terminal.session_id
                         AND resolution_order.source_kind = 'runtime_event'
                         AND resolution_order.source_id =
                             resolution_terminal.id
                        INNER JOIN protocol_item_append_order AS deferred_order
                          ON deferred_order.session_id = terminal.session_id
                         AND deferred_order.source_kind = 'runtime_event'
                         AND deferred_order.source_id = terminal.id
                        WHERE json_extract(
                                  resolution_terminal.msg_json,
                                  '$.kind'
                              ) = 'turn_terminal'
                          AND json_extract(
                                  resolution_terminal.msg_json,
                                  '$.terminal.outcome.kind'
                              ) = 'interrupted'
                          AND json_extract(
                                  resolution_terminal.msg_json,
                                  '$.terminal.outcome.cause'
                              ) IN (
                                  'approval_aborted',
                                  'user_stop',
                                  'tree_stopped'
                              )
                          AND resolution_order.append_position >
                              deferred_order.append_position
                    )
                    OR (
                        EXISTS (
                            SELECT 1
                            FROM protocol_runtime_events AS resolution_terminal
                            INNER JOIN protocol_item_append_order AS resolution_order
                              ON resolution_order.session_id =
                                 resolution_terminal.session_id
                             AND resolution_order.source_kind = 'runtime_event'
                             AND resolution_order.source_id =
                                 resolution_terminal.id
                            INNER JOIN protocol_item_append_order AS deferred_order
                              ON deferred_order.session_id = terminal.session_id
                             AND deferred_order.source_kind = 'runtime_event'
                             AND deferred_order.source_id = terminal.id
                            WHERE resolution_terminal.id =
                                  deferred.resolved_by_terminal_event_id
                              AND resolution_terminal.session_id =
                                  deferred.agent_session_id
                              AND resolution_terminal.turn_id <>
                                  deferred.agent_turn_id
                              AND json_extract(
                                    resolution_terminal.msg_json,
                                    '$.kind'
                                  ) = 'turn_terminal'
                              AND json_extract(
                                    resolution_terminal.msg_json,
                                    '$.terminal.outcome.kind'
                                  ) = 'interrupted'
                              AND json_extract(
                                    resolution_terminal.msg_json,
                                    '$.terminal.outcome.cause'
                                  ) IN (
                                      'approval_aborted',
                                      'user_stop',
                                      'tree_stopped'
                                  )
                              AND resolution_order.append_position >
                                  deferred_order.append_position
                        )
                    )
                    OR (
                        deferred.kind = 'crash_failed'
                        AND EXISTS (
                            SELECT 1
                            FROM protocol_runtime_events AS resolution_terminal
                            INNER JOIN protocol_item_append_order AS resolution_order
                              ON resolution_order.session_id =
                                 resolution_terminal.session_id
                             AND resolution_order.source_kind = 'runtime_event'
                             AND resolution_order.source_id =
                                 resolution_terminal.id
                            INNER JOIN protocol_item_append_order AS deferred_order
                              ON deferred_order.session_id = terminal.session_id
                             AND deferred_order.source_kind = 'runtime_event'
                             AND deferred_order.source_id = terminal.id
                            WHERE resolution_terminal.id =
                                  deferred.resolved_by_terminal_event_id
                              AND resolution_terminal.session_id =
                                  deferred.agent_session_id
                              AND resolution_terminal.turn_id <>
                                  deferred.agent_turn_id
                              AND json_extract(
                                    resolution_terminal.msg_json,
                                    '$.kind'
                                  ) = 'turn_terminal'
                              AND json_extract(
                                    resolution_terminal.msg_json,
                                    '$.terminal.outcome.kind'
                                  ) = 'interrupted'
                              AND resolution_order.append_position >
                                  deferred_order.append_position
                        )
                    )
                    OR (
                        deferred.resolved_by_terminal_event_id =
                            deferred.terminal_event_id
                        AND EXISTS (
                            WITH RECURSIVE
                            fenced_scope(
                                root_session_id,
                                stopped_session_id,
                                after_append_position,
                                session_id
                            ) AS (
                                SELECT
                                    fence.root_session_id,
                                    fence.stopped_session_id,
                                    fence.after_append_position,
                                    fence.stopped_session_id
                                FROM agent_tree_stop_fences AS fence
                                UNION ALL
                                SELECT
                                    fenced.root_session_id,
                                    fenced.stopped_session_id,
                                    fenced.after_append_position,
                                    child.child_session_id
                                FROM fenced_scope AS fenced
                                INNER JOIN session_spawn_edges AS child
                                  ON child.root_session_id =
                                     fenced.root_session_id
                                 AND child.parent_session_id =
                                     fenced.session_id
                            ),
                            deferred_scope(session_id) AS (
                                SELECT deferred.agent_session_id
                                UNION ALL
                                SELECT child.child_session_id
                                FROM session_spawn_edges AS child
                                INNER JOIN deferred_scope AS owner
                                  ON child.parent_session_id =
                                     owner.session_id
                            )
                            SELECT 1
                            FROM protocol_item_append_order AS terminal_order
                            INNER JOIN fenced_scope AS fenced
                              ON (
                                  fenced.session_id =
                                      deferred.agent_session_id
                                  OR EXISTS (
                                      SELECT 1
                                      FROM deferred_scope AS descendant
                                      WHERE descendant.session_id =
                                            fenced.stopped_session_id
                                  )
                              )
                            WHERE terminal_order.session_id =
                                  deferred.agent_session_id
                              AND terminal_order.source_kind =
                                  'runtime_event'
                              AND terminal_order.source_id =
                                  deferred.terminal_event_id
                              AND (
                                  SELECT MIN(turn_order.append_position)
                                  FROM protocol_item_append_order AS turn_order
                                  WHERE turn_order.session_id =
                                        deferred.agent_session_id
                                    AND turn_order.turn_id =
                                        deferred.agent_turn_id
                              ) <= fenced.after_append_position
                              AND terminal_order.append_position <=
                                  fenced.after_append_position
                        )
                    )
                    OR EXISTS (
                        WITH RECURSIVE
                        owner_scope(session_id) AS (
                            SELECT deferred.agent_session_id
                            UNION ALL
                            SELECT child.child_session_id
                            FROM session_spawn_edges AS child
                            INNER JOIN owner_scope AS owner
                              ON child.parent_session_id = owner.session_id
                        ),
                        fenced_scope(
                            root_session_id,
                            after_append_position,
                            session_id
                        ) AS (
                            SELECT
                                fence.root_session_id,
                                fence.after_append_position,
                                fence.stopped_session_id
                            FROM agent_tree_stop_fences AS fence
                            UNION ALL
                            SELECT
                                fenced.root_session_id,
                                fenced.after_append_position,
                                child.child_session_id
                            FROM fenced_scope AS fenced
                            INNER JOIN session_spawn_edges AS child
                              ON child.root_session_id =
                                 fenced.root_session_id
                             AND child.parent_session_id =
                                 fenced.session_id
                        )
                        SELECT 1
                        FROM protocol_runtime_events AS resolution_terminal
                        INNER JOIN protocol_item_append_order AS resolution_order
                          ON resolution_order.session_id =
                             resolution_terminal.session_id
                         AND resolution_order.source_kind = 'runtime_event'
                         AND resolution_order.source_id =
                             resolution_terminal.id
                        INNER JOIN owner_scope AS owner
                          ON owner.session_id =
                             resolution_terminal.session_id
                        INNER JOIN fenced_scope AS fenced
                          ON fenced.session_id =
                             resolution_terminal.session_id
                        WHERE resolution_terminal.id =
                              deferred.resolved_by_terminal_event_id
                          AND json_extract(
                                resolution_terminal.msg_json,
                                '$.kind'
                              ) = 'turn_terminal'
                          AND (
                              SELECT MIN(turn_order.append_position)
                              FROM protocol_item_append_order AS turn_order
                              WHERE turn_order.session_id =
                                    deferred.agent_session_id
                                AND turn_order.turn_id =
                                    deferred.agent_turn_id
                          ) <= fenced.after_append_position
                          AND resolution_order.append_position >
                              fenced.after_append_position
                    )
                )
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_deferred != 0 {
        return Err(StorageError::Message(format!(
            "V49 marker exists but {invalid_deferred} deferred-completion row(s) violate durable identity or state"
        )));
    }
    Ok(())
}

fn validate_v47_table_schema(
    connection: &Connection,
    table_name: &str,
) -> Result<(), StorageError> {
    let expected = canonical_recursive_session_spawn_edge_schema(table_name)?;
    let observed = normalized_schema_objects_for_table(connection, table_name)?;

    for ((object_type, object_name), expected_sql) in &expected {
        let Some(observed_sql) = observed.get(&(object_type.clone(), object_name.clone())) else {
            return Err(StorageError::Message(format!(
                "V47 marker exists but required {object_type} `{object_name}` for `{table_name}` is missing"
            )));
        };
        if observed_sql != expected_sql {
            return Err(StorageError::Message(format!(
                "V47 marker exists but {object_type} `{object_name}` has stale SQL"
            )));
        }
    }
    for (object_type, object_name) in observed.keys() {
        if !expected.contains_key(&(object_type.clone(), object_name.clone())) {
            return Err(StorageError::Message(format!(
                "V47 marker exists but unexpected {object_type} `{object_name}` owns `{table_name}`"
            )));
        }
    }
    Ok(())
}

type NormalizedSchemaObjects = BTreeMap<(String, String), String>;

fn canonical_recursive_session_spawn_edge_schema(
    table_name: &str,
) -> Result<NormalizedSchemaObjects, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE sessions (
             id TEXT PRIMARY KEY NOT NULL,
             project_id TEXT NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );
         CREATE TABLE session_spawn_edges (
             root_session_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             child_session_id TEXT PRIMARY KEY NOT NULL,
             agent_path TEXT NOT NULL,
             task_name TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_runtime_events (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             msg_json TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_history_items (
             id TEXT PRIMARY KEY NOT NULL,
             session_id TEXT NOT NULL,
             payload_json TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE protocol_item_append_order (
             append_position INTEGER PRIMARY KEY,
             session_id TEXT NOT NULL,
             source_kind TEXT NOT NULL,
             source_id TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V47_RECURSIVE_SESSION_SPAWN_EDGES)?;
    normalized_schema_objects_for_table(&connection, table_name)
}

fn normalized_schema_objects_for_table(
    connection: &Connection,
    table_name: &str,
) -> Result<NormalizedSchemaObjects, StorageError> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql
         FROM sqlite_master
         WHERE tbl_name = ?1
           AND sql IS NOT NULL
         ORDER BY type ASC, name ASC",
    )?;
    Ok(statement
        .query_map([table_name], |row| {
            let object_type = row.get::<_, String>(0)?;
            let object_name = row.get::<_, String>(1)?;
            let sql = row.get::<_, String>(2)?;
            Ok((
                (object_type, object_name),
                sql.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase(),
            ))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?)
}

fn validate_recursive_session_spawn_edge_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_shape = connection.query_row(
        "SELECT COUNT(*)
         FROM session_spawn_edges
         WHERE child_session_id = root_session_id
            OR parent_session_id = child_session_id
            OR spawn_order <= 0
            OR task_name = ''
            OR task_name = 'root'
            OR task_name GLOB '*[^a-z0-9_]*'
            OR agent_path NOT GLOB '/root/*'
            OR agent_path GLOB '*[^/a-z0-9_]*'
            OR agent_path GLOB '*//*'
            OR agent_path LIKE '%/'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_shape != 0 {
        return Err(StorageError::Message(format!(
            "V47 marker exists but {invalid_shape} session spawn edge(s) have a non-canonical shape"
        )));
    }

    let invalid_lineage = connection.query_row(
        "SELECT COUNT(*)
         FROM session_spawn_edges AS edge
         LEFT JOIN session_spawn_edges AS parent
           ON parent.root_session_id = edge.root_session_id
          AND parent.child_session_id = edge.parent_session_id
         WHERE (
             edge.parent_session_id = edge.root_session_id
             AND edge.agent_path <> '/root/' || edge.task_name
         )
         OR (
             edge.parent_session_id <> edge.root_session_id
             AND (
                 parent.child_session_id IS NULL
                 OR edge.agent_path <> parent.agent_path || '/' || edge.task_name
             )
         )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_lineage != 0 {
        return Err(StorageError::Message(format!(
            "V47 marker exists but {invalid_lineage} session spawn edge(s) have an orphan, cross-tree parent, or non-canonical parent path"
        )));
    }

    let invalid_ownership = connection.query_row(
        "SELECT COUNT(*)
         FROM session_spawn_edges AS edge
         INNER JOIN sessions AS root ON root.id = edge.root_session_id
         INNER JOIN sessions AS parent ON parent.id = edge.parent_session_id
         INNER JOIN sessions AS child ON child.id = edge.child_session_id
         WHERE root.project_id <> parent.project_id
            OR root.project_id <> child.project_id
            OR EXISTS (
                SELECT 1
                FROM session_spawn_edges AS owner
                WHERE owner.child_session_id = edge.root_session_id
            )
            OR EXISTS (
                SELECT 1
                FROM session_spawn_edges AS owned_tree
                WHERE owned_tree.root_session_id = edge.child_session_id
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_ownership != 0 {
        return Err(StorageError::Message(format!(
            "V47 marker exists but {invalid_ownership} session spawn edge(s) violate same-project single-tree ownership"
        )));
    }

    let duplicate_spawn_order = connection.query_row(
        "SELECT COUNT(*)
         FROM (
             SELECT root_session_id, spawn_order
             FROM session_spawn_edges
             GROUP BY root_session_id, spawn_order
             HAVING COUNT(*) <> 1
         )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if duplicate_spawn_order != 0 {
        return Err(StorageError::Message(
            "V47 marker exists but an agent tree has duplicate durable spawn orders".to_string(),
        ));
    }

    let unreachable = connection.query_row(
        "WITH RECURSIVE reachable(root_session_id, child_session_id) AS (
             SELECT root_session_id, child_session_id
             FROM session_spawn_edges
             WHERE parent_session_id = root_session_id
             UNION ALL
             SELECT child.root_session_id, child.child_session_id
             FROM session_spawn_edges AS child
             INNER JOIN reachable AS parent
               ON parent.root_session_id = child.root_session_id
              AND parent.child_session_id = child.parent_session_id
         )
         SELECT COUNT(*)
         FROM session_spawn_edges AS edge
         WHERE NOT EXISTS (
             SELECT 1
             FROM reachable
             WHERE reachable.root_session_id = edge.root_session_id
               AND reachable.child_session_id = edge.child_session_id
         )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if unreachable != 0 {
        return Err(StorageError::Message(format!(
            "V47 marker exists but {unreachable} session spawn edge(s) are not reachable from their root"
        )));
    }

    let max_descendants = connection.query_row(
        "SELECT COALESCE(MAX(descendant_count), 0)
         FROM (
             SELECT COUNT(*) AS descendant_count
             FROM session_spawn_edges
             GROUP BY root_session_id
         )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if max_descendants > 255 {
        return Err(StorageError::Message(format!(
            "V47 marker exists but one agent tree retains {max_descendants} descendants"
        )));
    }
    validate_agent_completion_handoff_data(connection)?;
    Ok(())
}

fn validate_agent_completion_handoff_data(connection: &Connection) -> Result<(), StorageError> {
    if schema_migration_applied(connection, DURABLE_AGENT_MAILBOX_VERSION)? {
        return validate_durable_agent_mailbox_links(connection);
    }
    let invalid_handoffs = connection.query_row(
        "SELECT COUNT(*)
         FROM agent_completion_handoffs AS handoff
         LEFT JOIN session_spawn_edges AS edge
           ON edge.child_session_id = handoff.child_session_id
          AND edge.parent_session_id = handoff.parent_session_id
         LEFT JOIN protocol_runtime_events AS terminal
           ON terminal.id = handoff.child_terminal_event_id
          AND terminal.session_id = handoff.child_session_id
          AND terminal.turn_id = handoff.child_turn_id
         LEFT JOIN protocol_history_items AS history
           ON history.id = handoff.parent_history_item_id
          AND history.session_id = handoff.parent_session_id
         LEFT JOIN session_spawn_edges AS parent_edge
           ON parent_edge.root_session_id = edge.root_session_id
          AND parent_edge.child_session_id = edge.parent_session_id
         WHERE edge.child_session_id IS NULL
            OR terminal.id IS NULL
            OR json_extract(terminal.msg_json, '$.kind') IS NOT 'turn_terminal'
            OR COALESCE(
                   json_extract(terminal.msg_json, '$.terminal.outcome.kind'),
                   ''
               )
                NOT IN ('completed', 'failed')
            OR history.id IS NULL
            OR json_extract(history.payload_json, '$.kind')
                IS NOT 'inter_agent_communication'
            OR json_extract(history.payload_json, '$.communication.author')
                IS NOT edge.agent_path
            OR json_extract(history.payload_json, '$.communication.recipient')
                IS NOT CASE
                           WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                           ELSE parent_edge.agent_path
                       END
            OR json_extract(history.payload_json, '$.communication.trigger_turn') IS NOT 0
            OR instr(
                   COALESCE(
                       json_extract(history.payload_json, '$.communication.content'),
                       ''
                   ),
                   'Message Type: FINAL_ANSWER' || char(10)
                         || 'Task name: ' ||
                         CASE
                             WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                             ELSE parent_edge.agent_path
                         END
                         || char(10) || 'Sender: ' || edge.agent_path
                         || char(10) || 'Payload:' || char(10)
               ) <> 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_handoffs != 0 {
        return Err(StorageError::Message(format!(
            "V47 marker exists but {invalid_handoffs} agent completion handoff receipt(s) do not link an eligible child terminal to the immediate parent's FINAL history"
        )));
    }
    Ok(())
}

fn validate_terminal_outcome_storage(connection: &Connection) -> Result<(), StorageError> {
    validate_v39_history_json(connection)?;
    if retired_durable_protocol_row_count(connection)? != 0 {
        return Err(StorageError::Message(
            "V39 marker exists but retired durable runtime or retry-history rows remain"
                .to_string(),
        ));
    }

    let mut runtime_statement = connection.prepare(
        "SELECT id, session_id, turn_id, sequence_no, msg_json, payload_sha256
         FROM protocol_runtime_events ORDER BY id ASC",
    )?;
    let runtime_rows = runtime_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut runtime_outcomes = BTreeMap::new();
    for row in runtime_rows {
        let (id, session_id, turn_id, sequence_no, msg_json, payload_sha256) = row?;
        let message = protocol_json_object(&msg_json, "runtime event", &id)?;
        if json_kind(&message, "runtime event", &id)? != "turn_terminal" {
            continue;
        }
        if payload_sha256 != sha256_text(&msg_json) {
            return Err(StorageError::Message(format!(
                "V39 marker exists but runtime terminal {id} has a stale payload hash"
            )));
        }
        let terminal = message
            .get("terminal")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "V39 marker exists but runtime terminal {id} has no terminal object"
                ))
            })?;
        reject_mixed_terminal_contract(
            terminal,
            &["status", "finish_reason", "interruption_cause", "summary"],
            "runtime terminal",
            &id,
        )?;
        let outcome = decode_current_terminal_outcome(
            terminal.get("outcome").ok_or_else(|| {
                StorageError::Message(format!(
                    "V39 marker exists but runtime terminal {id} has no outcome"
                ))
            })?,
            "runtime terminal",
            &id,
        )?;
        serde_json::from_value::<crate::session::DurableTurnTerminal>(serde_json::Value::Object(
            terminal.clone(),
        ))
        .map_err(|error| {
            StorageError::Message(format!(
                "V39 marker exists but runtime terminal {id} violates the current contract: {error}"
            ))
        })?;
        runtime_outcomes.insert((session_id, turn_id, sequence_no), outcome);
    }
    drop(runtime_statement);

    let mut turn_statement = connection.prepare(
        "SELECT id, session_id, turn_id, sequence_no, payload_json, payload_sha256
         FROM protocol_turn_items ORDER BY id ASC",
    )?;
    let turn_rows = turn_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    for row in turn_rows {
        let (id, session_id, turn_id, sequence_no, payload_json, payload_sha256) = row?;
        let payload = protocol_json_object(&payload_json, "turn item", &id)?;
        if json_kind(&payload, "turn item", &id)? != "terminal" {
            continue;
        }
        if payload_sha256 != sha256_text(&payload_json) {
            return Err(StorageError::Message(format!(
                "V39 marker exists but turn terminal {id} has a stale payload hash"
            )));
        }
        if payload
            .keys()
            .any(|field| field != "kind" && field != "outcome")
        {
            return Err(StorageError::Message(format!(
                "V39 marker exists but turn terminal {id} contains retired or unknown fields"
            )));
        }
        let outcome = decode_current_terminal_outcome(
            payload.get("outcome").ok_or_else(|| {
                StorageError::Message(format!(
                    "V39 marker exists but turn terminal {id} has no outcome"
                ))
            })?,
            "turn terminal",
            &id,
        )?;
        if let Some(runtime_outcome) = runtime_outcomes.get(&(session_id, turn_id, sequence_no))
            && *runtime_outcome != outcome
        {
            return Err(StorageError::Message(format!(
                "V39 marker exists but turn terminal {id} contradicts its runtime owner"
            )));
        }
    }
    drop(turn_statement);

    if orphaned_protocol_append_order_count(connection)? != 0 {
        return Err(StorageError::Message(
            "V39 marker exists but protocol append order contains orphaned source rows".to_string(),
        ));
    }
    Ok(())
}

fn retired_durable_protocol_row_count(connection: &Connection) -> Result<i64, StorageError> {
    connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM protocol_runtime_events
                 WHERE json_valid(msg_json)
                   AND json_extract(msg_json, '$.kind') IN (
                       'thread_configured', 'assistant_text_delta',
                       'reasoning_summary_delta', 'retry_scheduled',
                       'history_item_recorded'
                   ))
              + (SELECT COUNT(*) FROM protocol_history_items
                 WHERE json_valid(payload_json)
                   AND json_extract(payload_json, '$.kind') = 'retry_decision')",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::from)
}

fn orphaned_protocol_append_order_count(connection: &Connection) -> Result<i64, StorageError> {
    connection
        .query_row(
            "SELECT COUNT(*)
             FROM protocol_item_append_order AS append_order
             WHERE (append_order.source_kind = 'runtime_event'
                    AND NOT EXISTS (
                        SELECT 1 FROM protocol_runtime_events
                        WHERE id = append_order.source_id
                    ))
                OR (append_order.source_kind = 'history_item'
                    AND NOT EXISTS (
                        SELECT 1 FROM protocol_history_items
                        WHERE id = append_order.source_id
                    ))
                OR (append_order.source_kind = 'turn_item'
                    AND NOT EXISTS (
                        SELECT 1 FROM protocol_turn_items
                        WHERE id = append_order.source_id
                    ))",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::from)
}

fn canonicalize_raw_tool_call_history(connection: &Connection) -> Result<(), StorageError> {
    if !table_exists(connection, "protocol_history_items")? {
        return Ok(());
    }

    recover_missing_tool_call_response_lineage(connection)?;

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

fn recover_missing_tool_call_response_lineage(connection: &Connection) -> Result<(), StorageError> {
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
    let stored = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (id, session_id, turn_id, payload_json) in stored {
        let mut payload =
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
        if has_response_lineage {
            continue;
        }

        let candidates = response_lineage_candidates(connection, &session_id, &turn_id)?;
        let response_id = match candidates.as_slice() {
            [response_id] => response_id,
            [] => {
                return Err(StorageError::Message(format!(
                    "V37 tool-call history item {id} has no response_id and its turn has no uniquely recoverable assistant response lineage"
                )));
            }
            _ => {
                return Err(StorageError::Message(format!(
                    "V37 tool-call history item {id} has no response_id and its turn has {} distinct assistant response lineage candidates",
                    candidates.len()
                )));
            }
        };
        payload
            .as_object_mut()
            .expect("tool-call payload object")
            .insert(
                "response_id".to_string(),
                serde_json::Value::String(response_id.clone()),
            );
        let recovered_json = serde_json::to_string(&payload)?;
        connection.execute(
            "UPDATE protocol_history_items SET payload_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
            (&recovered_json, sha256_text(&recovered_json), &id),
        )?;
    }
    Ok(())
}

fn response_lineage_candidates(
    connection: &Connection,
    session_id: &str,
    turn_id: &str,
) -> Result<Vec<String>, StorageError> {
    let mut candidates = BTreeSet::new();
    let mut history = connection.prepare(
        "SELECT id, payload_json
         FROM protocol_history_items
         WHERE session_id = ?1 AND turn_id = ?2
         ORDER BY sequence_no ASC, id ASC",
    )?;
    let history_rows = history.query_map((session_id, turn_id), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in history_rows {
        let (id, payload_json) = row?;
        let payload =
            serde_json::from_str::<serde_json::Value>(&payload_json).map_err(|error| {
                StorageError::Message(format!(
                    "V37 cannot inspect protocol history item {id}: invalid JSON: {error}"
                ))
            })?;
        let Some(object) = payload.as_object() else {
            continue;
        };
        if matches!(
            object.get("kind").and_then(serde_json::Value::as_str),
            Some("assistant_message" | "tool_call")
        ) && let Some(response_id) = object
            .get("response_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        {
            candidates.insert(response_id.to_string());
        }
    }
    drop(history);

    let mut runtime = connection.prepare(
        "SELECT id, msg_json
         FROM protocol_runtime_events
         WHERE session_id = ?1 AND turn_id = ?2
         ORDER BY sequence_no ASC, id ASC",
    )?;
    let runtime_rows = runtime.query_map((session_id, turn_id), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in runtime_rows {
        let (id, msg_json) = row?;
        let payload = serde_json::from_str::<serde_json::Value>(&msg_json).map_err(|error| {
            StorageError::Message(format!(
                "V37 cannot inspect protocol runtime event {id}: invalid JSON: {error}"
            ))
        })?;
        let Some(object) = payload.as_object() else {
            continue;
        };
        if object.get("kind").and_then(serde_json::Value::as_str)
            == Some("assistant_message_committed")
            && let Some(response_id) = object
                .get("response_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
        {
            candidates.insert(response_id.to_string());
        }
    }
    Ok(candidates.into_iter().collect())
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

fn schema_migration_has_exact_name(
    connection: &Connection,
    version: i64,
    expected_name: &str,
) -> Result<bool, StorageError> {
    if !table_exists(connection, "moyai_schema_migrations")? {
        return Ok(false);
    }
    let name = connection
        .query_row(
            "SELECT name FROM moyai_schema_migrations WHERE version = ?1",
            [version],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(name.as_deref() == Some(expected_name))
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
    table_has_exact_check_domain(connection, table_name, "status", expected)
}

fn table_has_exact_access_mode_domain(
    connection: &Connection,
    table_name: &str,
    expected: &[&str],
) -> Result<bool, StorageError> {
    table_has_exact_check_domain(connection, table_name, "access_mode", expected)
}

fn table_has_exact_check_domain(
    connection: &Connection,
    table_name: &str,
    column_name: &str,
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
    let domains = check_domains_for_column(&sql, column_name);
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

fn check_domains_for_column(sql: &str, column_name: &str) -> Vec<BTreeSet<String>> {
    let tokens = tokenize_schema(sql);
    let mut domains = Vec::new();
    let mut index = 0;
    while index + 5 < tokens.len() {
        if !matches!(&tokens[index], SchemaToken::Word(word) if word == "check")
            || tokens[index + 1] != SchemaToken::LeftParen
            || !matches!(&tokens[index + 2], SchemaToken::Word(word) if word == column_name)
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
    run_foreign_keys_disabled_migration_action(connection, migration_name, |connection| {
        connection
            .execute_batch(migration_sql)
            .map_err(StorageError::from)
    })
}

fn run_foreign_keys_disabled_migration_action(
    connection: &Connection,
    migration_name: &str,
    action: impl FnOnce(&Connection) -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    if !connection.is_autocommit() {
        return Err(StorageError::Message(format!(
            "{migration_name} requires an autocommit connection"
        )));
    }
    let foreign_keys_before =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))?;
    connection.pragma_update(None, "foreign_keys", 0)?;

    let migration_result = action(connection);
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
        (Err(error), true) => Err(error),
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
        validate_agent_owner_resume_request_schema(&connection)
            .expect("current owner-resume schema");
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
        assert!(
            table_exists(&connection, "agent_owner_resume_requests").expect("owner-resume table")
        );
        assert!(
            table_exists(&connection, "agent_tree_stop_fences").expect("tree-stop fence table")
        );
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
            !schema_migration_applied(&connection, TYPED_HISTORY_SCOPE_VERSION)
                .expect("no V42 marker")
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
            schema_migration_applied(&connection, UNIQUE_TURN_TERMINAL_VERSION)
                .expect("V44 marker")
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
        let warning_turn_payload =
            serde_json::to_string(&crate::protocol::TurnItemPayload::Warning {
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
            !schema_migration_applied(&connection, TYPED_HISTORY_SCOPE_VERSION)
                .expect("no V42 marker")
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
                r#"INSERT INTO protocol_runtime_events
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
            schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("V37 marker")
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
            let value: serde_json::Value =
                serde_json::from_str(&payload).expect("canonical call JSON");
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
            schema_migration_applied(&connection, RAW_TOOL_CALL_HISTORY_VERSION)
                .expect("v37 marker")
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
            table_has_exact_access_mode_domain(
                &connection,
                "sessions",
                SESSION_ACCESS_MODE_DOMAIN,
            )
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
            table_has_exact_access_mode_domain(
                &connection,
                "sessions",
                SESSION_ACCESS_MODE_DOMAIN,
            )
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
        let outer: serde_json::Value =
            serde_json::from_str(&outer_json).expect("outer current JSON");
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

        let error = run(&connection)
            .expect_err("V47 marker must reject partial indexes as unique ownership");

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

        let error =
            run(&connection).expect_err("V47 marker must reject an always-disabled trigger");

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

        let error = run(&connection)
            .expect_err("V47 marker must reject a disabled handoff integrity trigger");
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
            !schema_migration_applied(&connection, AGENT_TREE_STOP_FENCES_VERSION)
                .expect("V49 absent")
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
            !schema_migration_applied(&connection, AGENT_TREE_STOP_FENCES_VERSION)
                .expect("V49 absent")
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
        validate_canonical_protocol_storage(&reopened)
            .expect("reopened watermark-backed fence audit");
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

        for (index, child_session_id) in [paired_child_id, missing_child_id].into_iter().enumerate()
        {
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
        let hash_error =
            run(&hash_connection).expect_err("stale V51 payload hash must fail closed");
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
        let terminal_json =
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
}
