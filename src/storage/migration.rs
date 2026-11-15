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
const V54_PERMISSION_RETRY_FENCES: &str =
    include_str!("../../migrations/V54__permission_retry_fences.sql");
const V55_DURABLE_SIDE_CHATS: &str = include_str!("../../migrations/V55__durable_side_chats.sql");
const V56_ROLLBACK_HARNESS_ORPHAN_RECOVERY: &str =
    include_str!("../../migrations/V56__rollback_harness_orphan_recovery.sql");
const V57_SESSION_ADMISSION_REVISIONS: &str =
    include_str!("../../migrations/V57__session_admission_revisions.sql");
const V58_EXACT_EXECUTION_INTERRUPT_REQUESTS: &str =
    include_str!("../../migrations/V58__exact_execution_interrupt_requests.sql");
const V59_SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW: &str =
    include_str!("../../migrations/V59__session_settings_revision_and_context_window.sql");
const V60_PROVIDER_CONNECTION_PROFILES: &str =
    include_str!("../../migrations/V60__provider_connection_profiles.sql");
const V61_SIDE_CHAT_SYSTEM_PROMPT: &str =
    include_str!("../../migrations/V61__side_chat_system_prompt.sql");
const V62_REMOTE_AGENT_JOBS: &str = include_str!("../../migrations/V62__remote_agent_jobs.sql");
const V63_DEVICE_OUTGOING_REFERENCES: &str =
    include_str!("../../migrations/V63__device_outgoing_references.sql");
const V64_REMOTE_NETWORK_RECEIPTS: &str =
    include_str!("../../migrations/V64__remote_network_receipts.sql");
const V65_REMOTE_ARTIFACTS: &str = include_str!("../../migrations/V65__remote_artifacts.sql");
const V66_SIDE_CHAT_ROUTE_KIND: &str =
    include_str!("../../migrations/V66__side_chat_route_kind.sql");
const V69_SIDE_CHAT_API_KEY_REFERENCES: &str =
    include_str!("../../migrations/V69__side_chat_api_key_references.sql");
const V70_CLM_CHECKPOINT: &str = include_str!("../../migrations/V70__clm_checkpoint.sql");
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
const PERMISSION_RETRY_FENCES_VERSION: i64 = 54;
const DURABLE_SIDE_CHATS_VERSION: i64 = 55;
const ROLLBACK_HARNESS_ORPHAN_RECOVERY_VERSION: i64 = 56;
const SESSION_ADMISSION_REVISIONS_VERSION: i64 = 57;
const EXACT_EXECUTION_INTERRUPT_REQUESTS_VERSION: i64 = 58;
const SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION: i64 = 59;
const PROVIDER_CONNECTION_PROFILES_VERSION: i64 = 60;
const SIDE_CHAT_SYSTEM_PROMPT_VERSION: i64 = 61;
const SIDE_CHAT_API_KEY_REFERENCES_VERSION: i64 = 69;
const CLM_CHECKPOINT_VERSION: i64 = 70;
/// The last version `run_to_current` applies. Advance it together with that endpoint so
/// startup keeps taking a backup before every later upgrade.
const CURRENT_SCHEMA_VERSION: i64 = CLM_CHECKPOINT_VERSION;
const CODEX_COMPACTION_CHECKPOINT_NAME: &str = "codex_compaction_checkpoint";
const RECURSIVE_SESSION_SPAWN_EDGES_NAME: &str = "recursive_session_spawn_edges";
const AGENT_OWNER_RESUME_REQUESTS_NAME: &str = "agent_owner_resume_requests";
const AGENT_TREE_STOP_FENCES_NAME: &str = "agent_tree_stop_fences";
const DURABLE_AGENT_MAILBOX_NAME: &str = "durable_agent_mailbox";
const DURABLE_TURN_INPUT_QUEUE_NAME: &str = "durable_turn_input_queue";
const HARNESS_TURN_IDENTITY_NAME: &str = "harness_turn_identity";
const AGENT_TRIGGER_TURN_CLAIMS_NAME: &str = "agent_trigger_turn_claims";
const PERMISSION_RETRY_FENCES_NAME: &str = "permission_retry_fences";
const DURABLE_SIDE_CHATS_NAME: &str = "durable_side_chats";
const ROLLBACK_HARNESS_ORPHAN_RECOVERY_NAME: &str = "rollback_harness_orphan_recovery";
const SESSION_ADMISSION_REVISIONS_NAME: &str = "session_admission_revisions";
const EXACT_EXECUTION_INTERRUPT_REQUESTS_NAME: &str = "exact_execution_interrupt_requests";
const SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_NAME: &str =
    "session_settings_revision_and_context_window";
const PROVIDER_CONNECTION_PROFILES_NAME: &str = "provider_connection_profiles";
const SIDE_CHAT_SYSTEM_PROMPT_NAME: &str = "side_chat_system_prompt";
const SIDE_CHAT_API_KEY_REFERENCES_NAME: &str = "side_chat_api_key_references";
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
    if schema_migration_applied(connection, SIDE_CHAT_SYSTEM_PROMPT_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        validate_side_chat_system_prompt_schema(connection)?;
        validate_side_chat_system_prompt_data(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        validate_exact_execution_interrupt_requests_schema(connection)?;
        validate_exact_execution_interrupt_requests_data(connection)?;
        validate_session_settings_revision_and_context_window_schema(connection)?;
        validate_session_settings_revision_and_context_window_data(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, PROVIDER_CONNECTION_PROFILES_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        validate_provider_connection_profiles_schema(connection)?;
        validate_provider_connection_profiles_data(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        validate_exact_execution_interrupt_requests_schema(connection)?;
        validate_exact_execution_interrupt_requests_data(connection)?;
        validate_session_settings_revision_and_context_window_schema(connection)?;
        validate_session_settings_revision_and_context_window_data(connection)?;
        return run_side_chat_system_prompt(connection);
    }
    if schema_migration_applied(
        connection,
        SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION,
    )? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        validate_exact_execution_interrupt_requests_schema(connection)?;
        validate_exact_execution_interrupt_requests_data(connection)?;
        validate_session_settings_revision_and_context_window_schema(connection)?;
        validate_session_settings_revision_and_context_window_data(connection)?;
        return run_provider_connection_profiles(connection);
    }
    if schema_migration_applied(connection, EXACT_EXECUTION_INTERRUPT_REQUESTS_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        validate_exact_execution_interrupt_requests_schema(connection)?;
        validate_exact_execution_interrupt_requests_data(connection)?;
        return run_session_settings_revision_and_context_window(connection);
    }
    if schema_migration_applied(connection, SESSION_ADMISSION_REVISIONS_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        return run_exact_execution_interrupt_requests(connection);
    }
    if schema_migration_applied(connection, ROLLBACK_HARNESS_ORPHAN_RECOVERY_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        return run_session_admission_revisions(connection);
    }
    if schema_migration_applied(connection, DURABLE_SIDE_CHATS_VERSION)? {
        return run_rollback_harness_orphan_recovery(connection);
    }
    if schema_migration_applied(connection, PERMISSION_RETRY_FENCES_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        run_durable_side_chats(connection)?;
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;
        return Ok(());
    }
    if schema_migration_applied(connection, AGENT_TRIGGER_TURN_CLAIMS_VERSION)? {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_harness_turn_identity_data(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        run_permission_retry_fences(connection)?;
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

pub(super) fn pending_upgrade_version(
    connection: &Connection,
) -> Result<Option<i64>, StorageError> {
    if table_exists(connection, "moyai_schema_migrations")? {
        let version: i64 = connection.query_row(
            "SELECT coalesce(max(version), 0) FROM moyai_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        return Ok((0..CURRENT_SCHEMA_VERSION)
            .contains(&version)
            .then_some(version));
    }
    let populated: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name NOT GLOB 'sqlite_*')",
        [], |row| row.get(0),
    )?;
    Ok(populated.then_some(0))
}

pub(crate) fn run_to_current(connection: &Connection) -> Result<(), StorageError> {
    // Recent migrations deliberately validate and commit one authority boundary at a time.
    // A product startup is nevertheless one user-visible migration attempt, so keep advancing
    // until the current endpoint is present instead of requiring one app restart per version.
    for _ in 0..=SIDE_CHAT_SYSTEM_PROMPT_VERSION {
        run(connection)?;
        if schema_migration_applied(connection, SIDE_CHAT_SYSTEM_PROMPT_VERSION)? {
            run_remote_agent_jobs(connection)?;
            run_device_outgoing_references(connection)?;
            run_remote_network_receipts(connection)?;
            run_remote_artifacts(connection)?;
            run_side_chat_route_kind(connection)?;
            run_shared_run_checkpoints(connection)?;
            run_shared_history_files(connection)?;
            run_side_chat_api_key_references(connection)?;
            return run_clm_checkpoint(connection);
        }
    }
    Err(StorageError::Message(format!(
        "storage migration did not reach current endpoint V{CURRENT_SCHEMA_VERSION}"
    )))
}

fn run_clm_checkpoint(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, CLM_CHECKPOINT_VERSION)? {
            connection.execute_batch(V70_CLM_CHECKPOINT)?;
        }
        if !schema_migration_has_exact_name(connection, CLM_CHECKPOINT_VERSION, "clm_checkpoint")? {
            return Err(StorageError::Message(
                "invalid CLM checkpoint migration marker".into(),
            ));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_side_chat_api_key_references(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_side_chat_system_prompt_schema(connection)?;
        validate_side_chat_system_prompt_data(connection)?;
        if !schema_migration_applied(connection, SIDE_CHAT_API_KEY_REFERENCES_VERSION)? {
            connection.execute_batch(V69_SIDE_CHAT_API_KEY_REFERENCES)?;
        }
        validate_side_chat_system_prompt_schema(connection)?;
        validate_side_chat_system_prompt_data(connection)?;
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_shared_history_files(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, 68)? {
            connection.execute_batch(include_str!(
                "../../migrations/V68__shared_history_files.sql"
            ))?;
        }
        if !schema_migration_has_exact_name(connection, 68, "shared_history_files")? {
            return Err(StorageError::Message(
                "invalid shared history file migration marker".into(),
            ));
        }
        connection.prepare(
            "SELECT session_id,original_path,local_path,sha256 FROM shared_history_files LIMIT 0",
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_shared_run_checkpoints(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, 67)? {
            connection.execute_batch(include_str!(
                "../../migrations/V67__shared_run_checkpoints.sql"
            ))?;
        }
        if !schema_migration_has_exact_name(connection, 67, "shared_run_checkpoints")? {
            return Err(StorageError::Message(
                "invalid shared checkpoint migration marker".into(),
            ));
        }
        connection.prepare("SELECT job_id, project_id, environment_id, session_id, turn_id, admission_id, state, checkpoint_json FROM shared_run_checkpoints LIMIT 0")?;
        let guarded: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = 'shared_run_fixed_access_mode')", [], |row| row.get(0),
        )?;
        if !guarded {
            return Err(StorageError::Message(
                "shared checkpoint access mode guard is missing".into(),
            ));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_side_chat_route_kind(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, 66)? {
            connection.execute_batch(V66_SIDE_CHAT_ROUTE_KIND)?;
        }
        if !schema_migration_has_exact_name(connection, 66, "side_chat_route_kind")? {
            return Err(StorageError::Message(
                "invalid side chat route migration marker".into(),
            ));
        }
        connection.prepare("SELECT provider_route_kind FROM side_chat_bindings LIMIT 0")?;
        let objects: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name='validate_side_chat_route_kind_before_update'",
            [], |row| row.get(0),
        )?;
        if objects != 1 {
            return Err(StorageError::Message(
                "side chat route schema is incomplete".into(),
            ));
        }
        validate_side_chat_system_prompt_schema(connection)?;
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_remote_artifacts(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, 65)? {
            connection.execute_batch(V65_REMOTE_ARTIFACTS)?;
        }
        if !schema_migration_has_exact_name(connection, 65, "remote_artifacts")? {
            return Err(StorageError::Message(
                "invalid remote artifact migration marker".into(),
            ));
        }
        connection.prepare("SELECT job_id,payload_json FROM remote_job_inputs LIMIT 0")?;
        connection
            .prepare("SELECT job_id,version,payload_json FROM remote_job_artifacts LIMIT 0")?;
        connection.prepare(
            "SELECT reference_id,job_id,version,payload_json FROM device_artifact_cache LIMIT 0",
        )?;
        let objects: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name IN ('remote_job_inputs_immutable','remote_job_artifacts_immutable','device_artifact_cache_immutable')", [], |row| row.get(0))?;
        if objects != 3 {
            return Err(StorageError::Message(
                "remote artifact schema is incomplete".into(),
            ));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_remote_network_receipts(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, 64)? {
            connection.execute_batch(V64_REMOTE_NETWORK_RECEIPTS)?;
        }
        if !schema_migration_has_exact_name(connection, 64, "remote_network_receipts")? {
            return Err(StorageError::Message(
                "invalid remote network receipt migration marker".into(),
            ));
        }
        connection.prepare("SELECT job_id,grant_id,settlement_delivered,last_attempt_ms FROM remote_network_receipts LIMIT 0")?;
        let objects: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE (type='index' AND name='remote_network_receipts_pending') OR (type='trigger' AND name='remote_network_receipts_immutable')", [], |row| row.get(0))?;
        if objects != 2 {
            return Err(StorageError::Message(
                "remote network receipt schema is incomplete".into(),
            ));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_remote_agent_jobs(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, 62)? {
            connection.execute_batch(V62_REMOTE_AGENT_JOBS)?;
        }
        if !schema_migration_has_exact_name(connection, 62, "remote_agent_jobs")? {
            return Err(StorageError::Message(
                "invalid remote job migration marker".into(),
            ));
        }
        connection.prepare("SELECT id, principal_id, profile_id, request_key, request_hash, scope_json, parent_json, prompt_preview, session_id, admitted_turn_id, created_at_ms FROM remote_agent_jobs LIMIT 0")?;
        connection.prepare("SELECT remote_temp_profile_id FROM projects LIMIT 0")?;
        let owned_objects: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE (type = 'index' AND name IN ('remote_agent_jobs_profile_created', 'remote_agent_jobs_created')) OR (type = 'trigger' AND name IN ('remote_agent_jobs_immutable', 'remote_temp_project_purpose_immutable'))", [], |row| row.get(0))?;
        if owned_objects != 4 {
            return Err(StorageError::Message(
                "remote job schema is incomplete".into(),
            ));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_device_outgoing_references(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, 63)? {
            connection.execute_batch(V63_DEVICE_OUTGOING_REFERENCES)?;
        }
        if !schema_migration_has_exact_name(connection, 63, "device_outgoing_references")? {
            return Err(StorageError::Message(
                "invalid outgoing reference migration marker".into(),
            ));
        }
        connection.prepare("SELECT id,session_id,turn_id,device_id,profile_id,root_task_id,request_key,prompt_hash,parent_grant_id,parent_job_id,payload_json,created_at_ms FROM device_outgoing_references LIMIT 0")?;
        let objects:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE (type='index' AND name IN ('device_outgoing_references_session_recent','device_outgoing_references_recent')) OR (type='trigger' AND name='device_outgoing_references_immutable')",[],|r|r.get(0))?;
        if objects != 3 {
            return Err(StorageError::Message(
                "outgoing reference schema is incomplete".into(),
            ));
        }
        Ok::<_, StorageError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
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
    }?;
    run_permission_retry_fences(connection)
}

fn run_permission_retry_fences(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, PERMISSION_RETRY_FENCES_VERSION)? {
            connection.execute_batch(V54_PERMISSION_RETRY_FENCES)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            PERMISSION_RETRY_FENCES_VERSION,
            PERMISSION_RETRY_FENCES_NAME,
        )? {
            return Err(StorageError::Message(
                "V54 permission retry-fence migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V54 permission retry-fence migration produced {foreign_key_errors} foreign-key violation(s)"
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
    run_durable_side_chats(connection)
}

fn run_durable_side_chats(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if !schema_migration_applied(connection, DURABLE_SIDE_CHATS_VERSION)? {
            connection.execute_batch(V55_DURABLE_SIDE_CHATS)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            DURABLE_SIDE_CHATS_VERSION,
            DURABLE_SIDE_CHATS_NAME,
        )? {
            return Err(StorageError::Message(
                "V55 durable side-chat migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V55 durable side-chat migration produced {foreign_key_errors} foreign-key violation(s)"
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
    run_rollback_harness_orphan_recovery(connection)
}

fn run_rollback_harness_orphan_recovery(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_agent_mailbox_data(connection)?;
        validate_durable_turn_input_queue_data(connection)?;
        validate_harness_turn_identity_schema(connection)?;
        validate_agent_trigger_turn_claims_schema(connection)?;
        validate_agent_trigger_turn_claims_data(connection)?;
        validate_permission_retry_fences_schema(connection)?;
        validate_permission_retry_fences_data(connection)?;
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;

        if !schema_migration_applied(connection, ROLLBACK_HARNESS_ORPHAN_RECOVERY_VERSION)? {
            repair_exact_rollback_harness_orphans(connection)?;
            connection.execute_batch(V56_ROLLBACK_HARNESS_ORPHAN_RECOVERY)?;
        }
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        validate_harness_turn_identity_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V56 rollback harness-orphan recovery produced {foreign_key_errors} foreign-key violation(s)"
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
    run_session_admission_revisions(connection)
}

fn run_session_admission_revisions(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_canonical_protocol_schema(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        if !schema_migration_applied(connection, SESSION_ADMISSION_REVISIONS_VERSION)? {
            connection.execute_batch(V57_SESSION_ADMISSION_REVISIONS)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            SESSION_ADMISSION_REVISIONS_VERSION,
            SESSION_ADMISSION_REVISIONS_NAME,
        )? {
            return Err(StorageError::Message(
                "V57 session-admission revision migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V57 session-admission revision migration produced {foreign_key_errors} foreign-key violation(s)"
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
    run_exact_execution_interrupt_requests(connection)
}

fn run_exact_execution_interrupt_requests(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_canonical_protocol_schema(connection)?;
        validate_rollback_harness_orphan_recovery_marker(connection)?;
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        if !schema_migration_applied(connection, EXACT_EXECUTION_INTERRUPT_REQUESTS_VERSION)? {
            connection.execute_batch(V58_EXACT_EXECUTION_INTERRUPT_REQUESTS)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            EXACT_EXECUTION_INTERRUPT_REQUESTS_VERSION,
            EXACT_EXECUTION_INTERRUPT_REQUESTS_NAME,
        )? {
            return Err(StorageError::Message(
                "V58 exact execution interrupt request migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_exact_execution_interrupt_requests_schema(connection)?;
        validate_exact_execution_interrupt_requests_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V58 exact execution interrupt request migration produced {foreign_key_errors} foreign-key violation(s)"
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

fn run_provider_connection_profiles(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_canonical_protocol_schema(connection)?;
        validate_durable_side_chats_schema(connection)?;
        validate_durable_side_chats_data(connection)?;
        validate_session_settings_revision_and_context_window_schema(connection)?;
        validate_session_settings_revision_and_context_window_data(connection)?;
        if !schema_migration_applied(connection, PROVIDER_CONNECTION_PROFILES_VERSION)? {
            connection.execute_batch(V60_PROVIDER_CONNECTION_PROFILES)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            PROVIDER_CONNECTION_PROFILES_VERSION,
            PROVIDER_CONNECTION_PROFILES_NAME,
        )? {
            return Err(StorageError::Message(
                "V60 provider connection profile migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_provider_connection_profiles_schema(connection)?;
        validate_provider_connection_profiles_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V60 provider connection profile migration produced {foreign_key_errors} foreign-key violation(s)"
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

fn run_side_chat_system_prompt(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_canonical_protocol_schema(connection)?;
        validate_provider_connection_profiles_schema(connection)?;
        validate_provider_connection_profiles_data(connection)?;
        if !schema_migration_applied(connection, SIDE_CHAT_SYSTEM_PROMPT_VERSION)? {
            connection.execute_batch(V61_SIDE_CHAT_SYSTEM_PROMPT)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            SIDE_CHAT_SYSTEM_PROMPT_VERSION,
            SIDE_CHAT_SYSTEM_PROMPT_NAME,
        )? {
            return Err(StorageError::Message(
                "V61 side-chat system-prompt migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_side_chat_system_prompt_schema(connection)?;
        validate_side_chat_system_prompt_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V61 side-chat system-prompt migration produced {foreign_key_errors} foreign-key violation(s)"
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

fn run_session_settings_revision_and_context_window(
    connection: &Connection,
) -> Result<(), StorageError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        validate_canonical_protocol_schema(connection)?;
        validate_session_admission_revisions_schema(connection)?;
        validate_session_admission_revisions_data(connection)?;
        validate_exact_execution_interrupt_requests_schema(connection)?;
        validate_exact_execution_interrupt_requests_data(connection)?;
        if !schema_migration_applied(
            connection,
            SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION,
        )? {
            connection.execute_batch(V59_SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW)?;
        }
        if !schema_migration_has_exact_name(
            connection,
            SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION,
            SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_NAME,
        )? {
            return Err(StorageError::Message(
                "V59 session-settings revision migration did not record its exact schema marker"
                    .to_string(),
            ));
        }
        validate_session_settings_revision_and_context_window_schema(connection)?;
        validate_session_settings_revision_and_context_window_data(connection)?;
        let foreign_key_errors =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if foreign_key_errors != 0 {
            return Err(StorageError::Message(format!(
                "V59 session-settings revision migration produced {foreign_key_errors} foreign-key violation(s)"
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
    run_provider_connection_profiles(connection)
}

fn validate_rollback_harness_orphan_recovery_marker(
    connection: &Connection,
) -> Result<(), StorageError> {
    if !schema_migration_has_exact_name(
        connection,
        ROLLBACK_HARNESS_ORPHAN_RECOVERY_VERSION,
        ROLLBACK_HARNESS_ORPHAN_RECOVERY_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V56 rollback harness-orphan recovery marker has a name other than `{ROLLBACK_HARNESS_ORPHAN_RECOVERY_NAME}`"
        )));
    }
    Ok(())
}

fn validate_session_admission_revisions_schema(
    connection: &Connection,
) -> Result<(), StorageError> {
    if !schema_migration_has_exact_name(
        connection,
        SESSION_ADMISSION_REVISIONS_VERSION,
        SESSION_ADMISSION_REVISIONS_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V57 session-admission revision marker has a name other than `{SESSION_ADMISSION_REVISIONS_NAME}`"
        )));
    }
    let expected_connection = canonical_session_admission_revisions_connection()?;
    let expected_table =
        normalized_schema_objects_for_table(&expected_connection, "session_admission_revisions")?;
    let observed_table =
        normalized_schema_objects_for_table(connection, "session_admission_revisions")?;
    if observed_table != expected_table {
        return Err(StorageError::Message(
            "V57 marker exists but the session-admission revision table has stale SQL".to_string(),
        ));
    }
    let expected_triggers = normalized_named_schema_objects(
        &expected_connection,
        "trigger",
        "session_admission_revisions_%",
    )?;
    let observed_triggers =
        normalized_named_schema_objects(connection, "trigger", "session_admission_revisions_%")?;
    if observed_triggers != expected_triggers {
        return Err(StorageError::Message(
            "V57 marker exists but the session-admission revision triggers have stale SQL"
                .to_string(),
        ));
    }
    Ok(())
}

fn canonical_session_admission_revisions_connection() -> Result<Connection, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE sessions (id TEXT PRIMARY KEY);
         CREATE TABLE protocol_turn_sequence_allocators (
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             next_sequence_no INTEGER NOT NULL,
             PRIMARY KEY (session_id, turn_id)
         );
         CREATE TABLE protocol_runtime_events (
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             sequence_no INTEGER NOT NULL
         );
         CREATE TABLE protocol_history_items (
             session_id TEXT NOT NULL,
             turn_id TEXT,
             scope_kind TEXT NOT NULL,
             sequence_no INTEGER NOT NULL
         );
         CREATE TABLE protocol_turn_items (
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             sequence_no INTEGER NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V57_SESSION_ADMISSION_REVISIONS)?;
    Ok(connection)
}

fn normalized_named_schema_objects(
    connection: &Connection,
    object_type: &str,
    name_pattern: &str,
) -> Result<BTreeMap<String, String>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT name, sql
         FROM sqlite_master
         WHERE type = ?1 AND name LIKE ?2 AND sql IS NOT NULL
         ORDER BY name ASC",
    )?;
    Ok(statement
        .query_map((object_type, name_pattern), |row| {
            let name = row.get::<_, String>(0)?;
            let sql = row.get::<_, String>(1)?;
            Ok((
                name,
                sql.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase(),
            ))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?)
}

fn validate_session_admission_revisions_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM sessions AS session
         LEFT JOIN session_admission_revisions AS revision
           ON revision.session_id = session.id
         WHERE revision.session_id IS NULL
            OR typeof(revision.revision) <> 'integer'
            OR revision.revision < 0
            OR revision.revision < (
                SELECT COUNT(*)
                FROM protocol_turn_sequence_allocators AS allocator
                WHERE allocator.session_id = session.id
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let orphan_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM session_admission_revisions AS revision
         LEFT JOIN sessions AS session ON session.id = revision.session_id
         WHERE session.id IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let orphan_allocators = connection.query_row(
        "SELECT COUNT(*)
         FROM protocol_turn_sequence_allocators AS allocator
         LEFT JOIN sessions AS session ON session.id = allocator.session_id
         LEFT JOIN session_admission_revisions AS revision
           ON revision.session_id = allocator.session_id
         WHERE session.id IS NULL OR revision.session_id IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let canonical_turns_without_allocator = connection.query_row(
        "SELECT COUNT(*)
         FROM (
             SELECT session_id, turn_id FROM protocol_runtime_events
             UNION
             SELECT session_id, turn_id FROM protocol_history_items
             WHERE scope_kind = 'turn' AND turn_id IS NOT NULL
             UNION
             SELECT session_id, turn_id FROM protocol_turn_items
         ) AS canonical_turn
         LEFT JOIN protocol_turn_sequence_allocators AS allocator
           ON allocator.session_id = canonical_turn.session_id
          AND allocator.turn_id = canonical_turn.turn_id
         WHERE allocator.turn_id IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_rows != 0
        || orphan_rows != 0
        || orphan_allocators != 0
        || canonical_turns_without_allocator != 0
    {
        return Err(StorageError::Message(format!(
            "V57 marker exists but {invalid_rows} session(s) lack a valid revision, {orphan_rows} revision row(s) are orphaned, {orphan_allocators} turn allocator row(s) lack a revision owner, and {canonical_turns_without_allocator} canonical turn(s) lack an allocator"
        )));
    }
    Ok(())
}

fn validate_exact_execution_interrupt_requests_schema(
    connection: &Connection,
) -> Result<(), StorageError> {
    if !schema_migration_has_exact_name(
        connection,
        EXACT_EXECUTION_INTERRUPT_REQUESTS_VERSION,
        EXACT_EXECUTION_INTERRUPT_REQUESTS_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V58 exact execution interrupt request marker has a name other than `{EXACT_EXECUTION_INTERRUPT_REQUESTS_NAME}`"
        )));
    }
    let expected_connection = canonical_exact_execution_interrupt_requests_connection()?;
    let expected_table = normalized_schema_objects_for_table(
        &expected_connection,
        "exact_execution_interrupt_requests",
    )?;
    let observed_table =
        normalized_schema_objects_for_table(connection, "exact_execution_interrupt_requests")?;
    if observed_table != expected_table {
        return Err(StorageError::Message(
            "V58 marker exists but the exact execution interrupt request table has stale SQL"
                .to_string(),
        ));
    }
    let expected_triggers = normalized_named_schema_objects(
        &expected_connection,
        "trigger",
        "exact_execution_interrupt_requests_%",
    )?;
    let observed_triggers = normalized_named_schema_objects(
        connection,
        "trigger",
        "exact_execution_interrupt_requests_%",
    )?;
    if observed_triggers != expected_triggers {
        return Err(StorageError::Message(
            "V58 marker exists but the exact execution interrupt request triggers have stale SQL"
                .to_string(),
        ));
    }
    Ok(())
}

fn canonical_exact_execution_interrupt_requests_connection() -> Result<Connection, StorageError> {
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
         CREATE TABLE session_admission_revisions (
             session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
             revision INTEGER NOT NULL
         );
         CREATE TABLE session_spawn_edges (child_session_id TEXT PRIMARY KEY);
         CREATE TABLE protocol_item_append_order (
             append_position INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL,
             turn_id TEXT
         );
         CREATE TABLE protocol_turn_sequence_allocators (
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             PRIMARY KEY (session_id, turn_id)
         );
         CREATE TABLE protocol_runtime_events (
             session_id TEXT NOT NULL,
             turn_id TEXT NOT NULL,
             msg_json TEXT NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V58_EXACT_EXECUTION_INTERRUPT_REQUESTS)?;
    Ok(connection)
}

fn validate_exact_execution_interrupt_requests_data(
    connection: &Connection,
) -> Result<(), StorageError> {
    let invalid_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM exact_execution_interrupt_requests AS request
         LEFT JOIN sessions AS session ON session.id = request.session_id
         LEFT JOIN session_admission_revisions AS revision
           ON revision.session_id = request.session_id
         WHERE session.id IS NULL
            OR revision.session_id IS NULL
            OR typeof(request.admission_revision) <> 'integer'
            OR request.admission_revision <= 0
            OR typeof(request.requested_at_ms) <> 'integer'
            OR request.requested_at_ms < 0
            OR request.cause NOT IN ('user_stop', 'agent_interrupted')
            OR session.status <> 'running'
            OR session.active_run_id IS NOT request.admission_id
            OR session.active_turn_id IS NOT request.turn_id
            OR session.active_run_lease_expires_at_ms IS NULL
            OR revision.revision IS NOT request.admission_revision
            OR (
                request.cause = 'user_stop'
                AND EXISTS (
                    SELECT 1
                    FROM session_spawn_edges AS edge
                    WHERE edge.child_session_id = request.session_id
                )
            )
            OR (
                request.cause = 'agent_interrupted'
                AND NOT EXISTS (
                    SELECT 1
                    FROM session_spawn_edges AS edge
                    WHERE edge.child_session_id = request.session_id
                )
            )
            OR NOT EXISTS (
                SELECT 1
                FROM protocol_turn_sequence_allocators AS allocator
                WHERE allocator.session_id = request.session_id
                  AND allocator.turn_id = request.turn_id
            )
            OR EXISTS (
                SELECT 1
                FROM protocol_runtime_events AS terminal
                WHERE terminal.session_id = request.session_id
                  AND terminal.turn_id = request.turn_id
                  AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let mut invalid_identities = 0usize;
    let mut statement = connection.prepare(
        "SELECT session_id, admission_id, turn_id
         FROM exact_execution_interrupt_requests
         ORDER BY session_id ASC",
    )?;
    let identities = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for identity in identities {
        let (session_id, admission_id, turn_id) = identity?;
        let canonical_ulid =
            |value: &str| Ulid::from_string(value).is_ok_and(|parsed| parsed.to_string() == value);
        if !canonical_ulid(&session_id)
            || !canonical_ulid(&admission_id)
            || !canonical_ulid(&turn_id)
        {
            invalid_identities += 1;
        }
    }
    if invalid_rows != 0 || invalid_identities != 0 {
        return Err(StorageError::Message(format!(
            "V58 marker exists but {invalid_rows} exact execution interrupt request row(s) have stale ownership and {invalid_identities} row(s) have invalid typed identities"
        )));
    }
    Ok(())
}

fn validate_session_settings_revision_and_context_window_schema(
    connection: &Connection,
) -> Result<(), StorageError> {
    if !schema_migration_has_exact_name(
        connection,
        SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_VERSION,
        SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V59 session-settings revision marker has a name other than `{SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW_NAME}`"
        )));
    }
    let column = connection
        .query_row(
            "SELECT type, \"notnull\", dflt_value, pk
             FROM pragma_table_info('sessions')
             WHERE name = 'session_settings_revision'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    if column != Some(("INTEGER".to_string(), 1, Some("0".to_string()), 0)) {
        return Err(StorageError::Message(
            "V59 marker exists but sessions.session_settings_revision has stale schema".to_string(),
        ));
    }
    let expected_clause = canonical_session_settings_revision_schema_clause()?;
    let observed_tables = normalized_named_schema_objects(connection, "table", "sessions")?;
    if !observed_tables
        .get("sessions")
        .is_some_and(|sql| sql.contains(&expected_clause))
    {
        return Err(StorageError::Message(
            "V59 marker exists but sessions.session_settings_revision has stale CHECK constraints"
                .to_string(),
        ));
    }

    Ok(())
}

fn canonical_session_settings_revision_schema_clause() -> Result<String, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY);
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V59_SESSION_SETTINGS_REVISION_AND_CONTEXT_WINDOW)?;
    let tables = normalized_named_schema_objects(&connection, "table", "sessions")?;
    let sql = tables.get("sessions").ok_or_else(|| {
        StorageError::Message("canonical V59 sessions table was not created".to_string())
    })?;
    let start = sql.find("session_settings_revision").ok_or_else(|| {
        StorageError::Message(
            "canonical V59 sessions table lacks session_settings_revision".to_string(),
        )
    })?;
    sql[start..]
        .strip_suffix(')')
        .map(str::trim_end)
        .map(ToString::to_string)
        .ok_or_else(|| {
            StorageError::Message("canonical V59 sessions table has stale SQL".to_string())
        })
}

fn validate_session_settings_revision_and_context_window_data(
    connection: &Connection,
) -> Result<(), StorageError> {
    let invalid_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM sessions
         WHERE typeof(session_settings_revision) <> 'integer'
            OR session_settings_revision < 0
            OR CASE
                 WHEN json_valid(model_parameters_json) = 0 THEN 1
                 WHEN json_type(model_parameters_json, '$') <> 'object' THEN 1
                 WHEN json_type(model_parameters_json, '$.context_window') IS NULL THEN 0
                 WHEN json_type(model_parameters_json, '$.context_window') = 'null' THEN 0
                 WHEN json_type(model_parameters_json, '$.context_window') <> 'integer' THEN 1
                 WHEN json_extract(model_parameters_json, '$.context_window') <= 0 THEN 1
                 WHEN json_extract(model_parameters_json, '$.context_window') > 4294967295 THEN 1
                 ELSE 0
               END = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_rows != 0 {
        return Err(StorageError::Message(format!(
            "V59 marker exists but {invalid_rows} session setting row(s) have an invalid revision or context window"
        )));
    }
    Ok(())
}

fn validate_provider_connection_profiles_schema(
    connection: &Connection,
) -> Result<(), StorageError> {
    if !schema_migration_has_exact_name(
        connection,
        DURABLE_SIDE_CHATS_VERSION,
        DURABLE_SIDE_CHATS_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V60 storage has a V55 side-chat marker other than `{DURABLE_SIDE_CHATS_NAME}`"
        )));
    }
    if !schema_migration_has_exact_name(
        connection,
        PROVIDER_CONNECTION_PROFILES_VERSION,
        PROVIDER_CONNECTION_PROFILES_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V60 provider connection profile marker has a name other than `{PROVIDER_CONNECTION_PROFILES_NAME}`"
        )));
    }
    validate_provider_connection_profiles_session_schema(connection)?;

    let expected_connection = canonical_provider_connection_profiles_connection()?;
    let expected_side_chats =
        normalized_schema_objects_for_table(&expected_connection, "side_chat_bindings")?;
    let observed_side_chats =
        normalized_schema_objects_for_table(connection, "side_chat_bindings")?;
    if observed_side_chats != expected_side_chats {
        return Err(StorageError::Message(
            "V60 marker exists but side_chat_bindings does not own the canonical provider-profile schema"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_provider_connection_profiles_session_schema(
    connection: &Connection,
) -> Result<(), StorageError> {
    let column = connection
        .query_row(
            "SELECT type, \"notnull\", dflt_value, pk
             FROM pragma_table_info('sessions')
             WHERE name = 'provider_connection_json'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    if column != Some(("TEXT".to_string(), 0, None, 0)) {
        return Err(StorageError::Message(
            "V60 marker exists but sessions.provider_connection_json has stale schema".to_string(),
        ));
    }

    let expected_connection = canonical_provider_connection_profiles_connection()?;
    let expected_clause =
        canonical_provider_connection_profiles_schema_clause(&expected_connection)?;
    let observed_tables = normalized_named_schema_objects(connection, "table", "sessions")?;
    if !observed_tables
        .get("sessions")
        .is_some_and(|sql| sql.contains(&expected_clause))
    {
        return Err(StorageError::Message(
            "V60 marker exists but sessions.provider_connection_json has stale CHECK constraints"
                .to_string(),
        ));
    }

    let expected_session_triggers = normalized_named_schema_objects(
        &expected_connection,
        "trigger",
        "validate_session_provider_connection_%",
    )?;
    let observed_session_triggers = normalized_named_schema_objects(
        connection,
        "trigger",
        "validate_session_provider_connection_%",
    )?;
    if observed_session_triggers != expected_session_triggers {
        return Err(StorageError::Message(
            "V60 marker exists but session provider-connection validation triggers have stale SQL"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_side_chat_system_prompt_schema(connection: &Connection) -> Result<(), StorageError> {
    if !schema_migration_has_exact_name(
        connection,
        DURABLE_SIDE_CHATS_VERSION,
        DURABLE_SIDE_CHATS_NAME,
    )? || !schema_migration_has_exact_name(
        connection,
        PROVIDER_CONNECTION_PROFILES_VERSION,
        PROVIDER_CONNECTION_PROFILES_NAME,
    )? {
        return Err(StorageError::Message(
            "V61 storage has a stale side-chat or provider-profile predecessor marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        SIDE_CHAT_SYSTEM_PROMPT_VERSION,
        SIDE_CHAT_SYSTEM_PROMPT_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V61 side-chat system-prompt marker has a name other than `{SIDE_CHAT_SYSTEM_PROMPT_NAME}`"
        )));
    }
    validate_provider_connection_profiles_session_schema(connection)?;

    let column = connection
        .query_row(
            "SELECT type, \"notnull\", dflt_value, pk
             FROM pragma_table_info('side_chat_bindings')
             WHERE name = 'system_prompt'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    if column != Some(("TEXT".to_string(), 1, Some("''".to_string()), 0)) {
        return Err(StorageError::Message(
            "V61 marker exists but side_chat_bindings.system_prompt has stale schema".to_string(),
        ));
    }

    let expected_connection = canonical_side_chat_system_prompt_connection()?;
    // V66 extends this same table. Keep the exact released V61 baseline for
    // older stores, and compare the full forward schema once V66 is recorded.
    if schema_migration_applied(connection, 66)? {
        if !schema_migration_has_exact_name(connection, 66, "side_chat_route_kind")? {
            return Err(StorageError::Message(
                "invalid side chat route migration marker".into(),
            ));
        }
        expected_connection.execute_batch(V66_SIDE_CHAT_ROUTE_KIND)?;
    }
    if schema_migration_applied(connection, SIDE_CHAT_API_KEY_REFERENCES_VERSION)? {
        if !schema_migration_has_exact_name(connection, 66, "side_chat_route_kind")?
            || !schema_migration_has_exact_name(
                connection,
                SIDE_CHAT_API_KEY_REFERENCES_VERSION,
                SIDE_CHAT_API_KEY_REFERENCES_NAME,
            )?
        {
            return Err(StorageError::Message(
                "invalid side chat API-key reference migration marker or predecessor".into(),
            ));
        }
        expected_connection.execute_batch(V69_SIDE_CHAT_API_KEY_REFERENCES)?;
    }
    let expected_side_chats =
        normalized_schema_objects_for_table(&expected_connection, "side_chat_bindings")?;
    let observed_side_chats =
        normalized_schema_objects_for_table(connection, "side_chat_bindings")?;
    if observed_side_chats != expected_side_chats {
        return Err(StorageError::Message(
            "V61 marker exists but side_chat_bindings does not own the canonical system-prompt schema"
                .to_string(),
        ));
    }
    Ok(())
}

fn canonical_provider_connection_profiles_connection() -> Result<Connection, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE sessions (
             id TEXT PRIMARY KEY,
             project_id TEXT NOT NULL,
             status TEXT NOT NULL,
             model_name TEXT NOT NULL,
             base_url TEXT NOT NULL,
             active_run_id TEXT,
             active_turn_id TEXT,
             active_run_lease_expires_at_ms INTEGER
         );
         CREATE TABLE session_spawn_edges (
             root_session_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             child_session_id TEXT NOT NULL
         );
         CREATE TABLE protocol_history_items (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             turn_id TEXT,
             payload_json TEXT NOT NULL
         );
         CREATE TABLE protocol_item_append_order (
             append_position INTEGER PRIMARY KEY,
             session_id TEXT NOT NULL,
             source_kind TEXT NOT NULL,
             source_id TEXT NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V55_DURABLE_SIDE_CHATS)?;
    connection.execute_batch(V60_PROVIDER_CONNECTION_PROFILES)?;
    Ok(connection)
}

fn canonical_side_chat_system_prompt_connection() -> Result<Connection, StorageError> {
    let connection = canonical_provider_connection_profiles_connection()?;
    connection.execute_batch(V61_SIDE_CHAT_SYSTEM_PROMPT)?;
    Ok(connection)
}

fn canonical_provider_connection_profiles_schema_clause(
    connection: &Connection,
) -> Result<String, StorageError> {
    let tables = normalized_named_schema_objects(connection, "table", "sessions")?;
    let sql = tables.get("sessions").ok_or_else(|| {
        StorageError::Message("canonical V60 sessions table was not created".to_string())
    })?;
    let start = sql.find("provider_connection_json").ok_or_else(|| {
        StorageError::Message(
            "canonical V60 sessions table lacks provider_connection_json".to_string(),
        )
    })?;
    sql[start..]
        .strip_suffix(')')
        .map(str::trim_end)
        .map(ToString::to_string)
        .ok_or_else(|| StorageError::Message("canonical V60 sessions table has stale SQL".into()))
}

fn validate_provider_connection_profiles_data(connection: &Connection) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, provider_connection_json
         FROM sessions
         WHERE provider_connection_json IS NOT NULL
         ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (session_id, json) = row?;
        let value = serde_json::from_str::<serde_json::Value>(&json).map_err(|_| {
            StorageError::Message(format!(
                "V60 session `{session_id}` has malformed provider connection JSON"
            ))
        })?;
        let object = value.as_object().ok_or_else(|| {
            StorageError::Message(format!(
                "V60 session `{session_id}` provider connection must be a JSON object"
            ))
        })?;
        if object.len() != 3
            || !object.contains_key("profile")
            || !object.contains_key("api_key_env")
            || !object.contains_key("extra_headers")
        {
            return Err(StorageError::Message(format!(
                "V60 session `{session_id}` provider connection has missing or unknown fields"
            )));
        }
        let provider_connection =
            serde_json::from_value::<crate::session::SessionProviderConnection>(value).map_err(
                |error| {
                    StorageError::Message(format!(
                        "V60 session `{session_id}` provider connection does not match the typed contract: {error}"
                    ))
                },
            )?;
        provider_connection.validate().map_err(|message| {
            StorageError::Message(format!(
                "V60 session `{session_id}` has an invalid provider connection: {message}"
            ))
        })?;
    }

    let invalid_side_chats = connection.query_row(
        "SELECT COUNT(*)
         FROM side_chat_bindings AS binding
         LEFT JOIN sessions AS owner ON owner.id = binding.owner_session_id
         LEFT JOIN sessions AS conversation
           ON conversation.id = binding.conversation_session_id
         WHERE owner.id IS NULL
            OR conversation.id IS NULL
            OR binding.owner_session_id = binding.conversation_session_id
            OR owner.project_id <> conversation.project_id
            OR conversation.model_name <> binding.model
            OR conversation.base_url <> binding.base_url
            OR json_extract(conversation.provider_connection_json, '$.profile')
                 IS NOT binding.provider_profile
            OR (?1 = 0
                AND json_type(conversation.provider_connection_json, '$.api_key_env') <> 'null')
            OR json_type(conversation.provider_connection_json, '$.extra_headers') <> 'object'
            OR EXISTS (
                SELECT 1
                FROM json_each(conversation.provider_connection_json, '$.extra_headers')
            )
            OR EXISTS (
                SELECT 1 FROM side_chat_bindings AS parent_binding
                WHERE parent_binding.conversation_session_id = binding.owner_session_id
            )
            OR EXISTS (
                SELECT 1 FROM session_spawn_edges AS edge
                WHERE edge.root_session_id = binding.conversation_session_id
                   OR edge.parent_session_id = binding.conversation_session_id
                   OR edge.child_session_id = binding.conversation_session_id
            )
            OR binding.context_scope <> 'general'
            OR binding.context_window <= 0
            OR binding.context_window > 4294967295
            OR binding.max_output_tokens <= 0
            OR binding.max_output_tokens > 4294967295
            OR binding.request_timeout_ms <= 0
            OR binding.connect_timeout_ms <= 0
            OR binding.max_retries < 0
            OR binding.max_retries > 255
            OR binding.supports_images NOT IN (0, 1)
            OR binding.supports_tools NOT IN (0, 1)
            OR binding.supports_reasoning NOT IN (0, 1)
            OR length(CAST(binding.persisted_draft AS BLOB)) > 1048576
            OR binding.draft_revision < 0
            OR binding.request_generation < 0
            OR binding.delete_requested_at_ms < 0
            OR binding.created_at_ms < 0
            OR binding.updated_at_ms < binding.created_at_ms",
        [schema_migration_applied(
            connection,
            SIDE_CHAT_API_KEY_REFERENCES_VERSION,
        )?],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_side_chats != 0 {
        return Err(StorageError::Message(format!(
            "V60 marker exists but {invalid_side_chats} side chat binding row(s) violate canonical ownership, provider-profile, or revision constraints"
        )));
    }

    let mut statement = connection.prepare(
        "SELECT id, owner_session_id, conversation_session_id, provider_profile
         FROM side_chat_bindings",
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
        let (id, owner_session_id, conversation_session_id, profile) = row?;
        id.parse::<crate::storage::SideChatId>().map_err(|error| {
            StorageError::Message(format!(
                "V60 side chat binding has invalid id `{id}`: {error}"
            ))
        })?;
        owner_session_id
            .parse::<crate::session::SessionId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V60 side chat binding has invalid owner session id `{owner_session_id}`: {error}"
                ))
            })?;
        conversation_session_id
            .parse::<crate::session::SessionId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V60 side chat binding has invalid conversation session id `{conversation_session_id}`: {error}"
                ))
            })?;
        if crate::config::ProviderProfile::parse(&profile)
            .is_none_or(|parsed| parsed.as_str() != profile)
        {
            return Err(StorageError::Message(format!(
                "V60 side chat binding `{id}` has invalid provider profile `{profile}`"
            )));
        }
    }
    Ok(())
}

fn validate_side_chat_system_prompt_data(connection: &Connection) -> Result<(), StorageError> {
    validate_provider_connection_profiles_data(connection)?;

    let invalid_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM side_chat_bindings
         WHERE typeof(system_prompt) <> 'text'
            OR length(system_prompt) > 16384",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_rows != 0 {
        return Err(StorageError::Message(format!(
            "V61 marker exists but {invalid_rows} side chat system prompt row(s) violate the text or character-limit contract"
        )));
    }

    let mut statement = connection.prepare(
        "SELECT id, system_prompt
         FROM side_chat_bindings
         ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, system_prompt) = row?;
        if system_prompt.chars().count()
            > crate::system_prompt::MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS
        {
            return Err(StorageError::Message(format!(
                "V61 side chat binding `{id}` has an oversized system prompt"
            )));
        }
        if system_prompt.trim() != system_prompt {
            return Err(StorageError::Message(format!(
                "V61 side chat binding `{id}` has a non-canonical system prompt boundary"
            )));
        }
    }
    Ok(())
}

fn repair_exact_rollback_harness_orphans(connection: &Connection) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT run.id, run.session_id, run.protocol_turn_id,
                run.canonical_terminal_runtime_event_id
         FROM harness_runs AS run
         INNER JOIN sessions AS session ON session.id = run.session_id
         WHERE run.mode = 'native_runtime'
           AND run.session_id IS NOT NULL
           AND run.protocol_turn_id IS NOT NULL
           AND session.status <> 'running'
           AND session.active_turn_id IS NOT run.protocol_turn_id
           AND NOT EXISTS (
               SELECT 1 FROM turn_steer_inputs AS input
               WHERE input.session_id = run.session_id AND input.turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM protocol_history_items AS history
               WHERE history.session_id = run.session_id AND history.turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM protocol_runtime_events AS event
               WHERE event.session_id = run.session_id AND event.turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM protocol_turn_items AS item
               WHERE item.session_id = run.session_id AND item.turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM protocol_item_append_order AS append_order
               WHERE append_order.session_id = run.session_id
                 AND append_order.turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM protocol_turn_sequence_allocators AS allocator
               WHERE allocator.session_id = run.session_id AND allocator.turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM agent_mailbox_messages AS mailbox
               WHERE mailbox.recipient_session_id = run.session_id
                 AND mailbox.delivered_turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM agent_completion_handoffs AS handoff
               WHERE handoff.child_session_id = run.session_id
                 AND handoff.child_turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM agent_deferred_completions AS deferred
               WHERE deferred.agent_session_id = run.session_id
                 AND deferred.agent_turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM agent_owner_resume_requests AS resume
               WHERE resume.owner_session_id = run.session_id
                 AND resume.claimed_turn_id = run.protocol_turn_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM agent_trigger_turn_claims AS claim
               WHERE claim.recipient_session_id = run.session_id
                 AND claim.turn_id = run.protocol_turn_id
           )
           AND (
               (
                   run.status = '\"started\"'
                   AND run.completed_at_ms IS NULL
                   AND run.canonical_terminal_runtime_event_id IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM harness_events AS terminalized
                       WHERE terminalized.run_id = run.id
                         AND json_extract(terminalized.kind, '$') = 'run_terminalized'
                   )
               )
               OR
               (
                   run.status IN ('\"pass\"', '\"fail\"', '\"blocked\"')
                   AND run.completed_at_ms IS NOT NULL
                   AND run.canonical_terminal_runtime_event_id IS NOT NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM protocol_runtime_events AS terminal
                       WHERE terminal.id = run.canonical_terminal_runtime_event_id
                   )
                   AND (
                       SELECT COUNT(*)
                       FROM harness_events AS terminalized
                       WHERE terminalized.run_id = run.id
                         AND json_extract(terminalized.kind, '$') = 'run_terminalized'
                         AND terminalized.created_at_ms = run.completed_at_ms
                         AND json_extract(terminalized.payload_json, '$.type') = 'generic'
                         AND json_extract(terminalized.payload_json, '$.data.kind') = 'turn_terminal'
                         AND json_extract(terminalized.payload_json, '$.data.session_id') = run.session_id
                         AND run.status = CASE json_extract(
                             terminalized.payload_json,
                             '$.data.terminal.outcome.kind'
                         )
                             WHEN 'completed' THEN '\"pass\"'
                             WHEN 'failed' THEN '\"fail\"'
                             WHEN 'interrupted' THEN '\"blocked\"'
                             ELSE NULL
                         END
                   ) = 1
               )
           )
         ORDER BY run.id ASC",
    )?;
    let candidates = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (run_id, session_id, turn_id, terminal_event_id) in &candidates {
        run_id
            .parse::<crate::harness::HarnessRunId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V56 recovery candidate has invalid harness run id `{run_id}`: {error}"
                ))
            })?;
        let parsed_session_id = session_id.parse::<crate::session::SessionId>().map_err(
            |error| {
                StorageError::Message(format!(
                    "V56 recovery candidate {run_id} has invalid session id `{session_id}`: {error}"
                ))
            },
        )?;
        turn_id
            .parse::<crate::protocol::TurnId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V56 recovery candidate {run_id} has invalid turn id `{turn_id}`: {error}"
                ))
            })?;
        if let Some(terminal_event_id) = terminal_event_id {
            terminal_event_id
                .parse::<crate::protocol::RuntimeEventId>()
                .map_err(|error| {
                    StorageError::Message(format!(
                        "V56 recovery candidate {run_id} has invalid terminal event id `{terminal_event_id}`: {error}"
                    ))
                })?;
            validate_v56_terminalized_harness_event(connection, run_id, parsed_session_id)?;
        }
    }

    for (run_id, _, _, _) in candidates {
        for table_name in [
            "harness_replay_reports",
            "harness_gate_results",
            "harness_contracts",
            "harness_artifacts",
            "harness_events",
        ] {
            connection.execute(
                &format!("DELETE FROM {table_name} WHERE run_id = ?1"),
                [&run_id],
            )?;
        }
        connection.execute("DELETE FROM harness_runs WHERE id = ?1", [&run_id])?;
    }
    Ok(())
}

fn validate_v56_terminalized_harness_event(
    connection: &Connection,
    run_id: &str,
    expected_session_id: crate::session::SessionId,
) -> Result<(), StorageError> {
    let mut statement = connection.prepare(
        "SELECT id, run_id, sequence_no, kind, payload_json,
                contract_refs_json, artifact_refs_json, parent_event_id,
                payload_sha256, created_at_ms
         FROM harness_events
         WHERE run_id = ?1
           AND json_extract(kind, '$') = 'run_terminalized'
         ORDER BY sequence_no ASC, id ASC",
    )?;
    let events = statement
        .query_map([run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let [
        (
            event_id,
            stored_run_id,
            sequence_no,
            kind_json,
            payload_json,
            contract_refs_json,
            artifact_refs_json,
            parent_event_id,
            payload_sha256,
            created_at_ms,
        ),
    ] = events.as_slice()
    else {
        return Err(StorageError::Message(format!(
            "V56 recovery candidate {run_id} has no exact terminalized harness payload"
        )));
    };
    event_id
        .parse::<crate::harness::HarnessEventId>()
        .map_err(|error| {
            StorageError::Message(format!(
                "V56 recovery candidate {run_id} has invalid harness event id `{event_id}`: {error}"
            ))
        })?;
    crate::harness::event_store::validate_stored_event_hash(
        event_id,
        stored_run_id,
        *sequence_no,
        kind_json,
        payload_json,
        contract_refs_json,
        artifact_refs_json,
        parent_event_id.as_deref(),
        payload_sha256,
        *created_at_ms,
    )
    .map_err(|error| {
        StorageError::Message(format!(
            "V56 recovery candidate {run_id} has a stale terminalized harness payload hash: {error}"
        ))
    })?;
    let payload = serde_json::from_str::<crate::harness::HarnessEventPayload>(payload_json)
        .map_err(|error| {
            StorageError::Message(format!(
                "V56 recovery candidate {run_id} has an invalid typed terminalized harness payload: {error}"
            ))
        })?;
    let crate::harness::HarnessEventPayload::Generic(run_event) = payload else {
        return Err(StorageError::Message(format!(
            "V56 recovery candidate {run_id} terminalized payload is not generic"
        )));
    };
    let run_event =
        serde_json::from_value::<crate::session::RunEvent>(run_event).map_err(|error| {
            StorageError::Message(format!(
                "V56 recovery candidate {run_id} has an invalid typed terminal run event: {error}"
            ))
        })?;
    let crate::session::RunEvent::TurnTerminal {
        session_id: event_session_id,
        ..
    } = run_event
    else {
        return Err(StorageError::Message(format!(
            "V56 recovery candidate {run_id} payload is not a terminal run event"
        )));
    };
    if event_session_id != expected_session_id {
        return Err(StorageError::Message(format!(
            "V56 recovery candidate {run_id} terminalized payload names a different session"
        )));
    }
    Ok(())
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
        "clm_checkpoint",
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
        if *field != "clm_checkpoint" && !object.contains_key(*field) {
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
    current
        .validate_compaction_checkpoint()
        .map_err(StorageError::Message)?;
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

fn validate_permission_retry_fences_schema(connection: &Connection) -> Result<(), StorageError> {
    if !schema_migration_applied(connection, PERMISSION_RETRY_FENCES_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V54 permission retry-fence marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        PERMISSION_RETRY_FENCES_VERSION,
        PERMISSION_RETRY_FENCES_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V54 permission retry-fence marker has a name other than `{PERMISSION_RETRY_FENCES_NAME}`"
        )));
    }
    let expected_connection = canonical_permission_retry_fences_connection()?;
    let expected =
        normalized_schema_objects_for_table(&expected_connection, "permission_retry_fences")?;
    let observed = normalized_schema_objects_for_table(connection, "permission_retry_fences")?;
    for ((object_type, object_name), expected_sql) in &expected {
        let Some(observed_sql) = observed.get(&(object_type.clone(), object_name.clone())) else {
            return Err(StorageError::Message(format!(
                "V54 marker exists but required {object_type} `{object_name}` is missing"
            )));
        };
        if observed_sql != expected_sql {
            return Err(StorageError::Message(format!(
                "V54 marker exists but {object_type} `{object_name}` has stale SQL"
            )));
        }
    }
    for (object_type, object_name) in observed.keys() {
        if !expected.contains_key(&(object_type.clone(), object_name.clone())) {
            return Err(StorageError::Message(format!(
                "V54 marker exists but unexpected {object_type} `{object_name}` owns `permission_retry_fences`"
            )));
        }
    }
    Ok(())
}

fn canonical_permission_retry_fences_connection() -> Result<Connection, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE sessions (id TEXT PRIMARY KEY);
         CREATE TABLE protocol_history_items (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
             payload_json TEXT NOT NULL
         );
         CREATE TABLE protocol_item_append_order (
             session_id TEXT NOT NULL,
             source_kind TEXT NOT NULL,
             source_id TEXT NOT NULL,
             append_position INTEGER NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V54_PERMISSION_RETRY_FENCES)?;
    Ok(connection)
}

fn validate_permission_retry_fences_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM permission_retry_fences AS fence
         LEFT JOIN sessions AS root
           ON root.id = fence.root_session_id
         LEFT JOIN protocol_history_items AS authority
           ON authority.id = fence.authority_history_item_id
          AND authority.session_id = fence.root_session_id
         LEFT JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = fence.root_session_id
          AND append_order.source_kind = 'history_item'
          AND append_order.source_id = fence.authority_history_item_id
         WHERE root.id IS NULL
            OR authority.id IS NULL
            OR append_order.source_id IS NULL
            OR json_extract(authority.payload_json, '$.kind')
                NOT IN ('user_turn', 'steer_turn')
            OR fence.family_version <= 0
            OR fence.family_version > 4294967295
            OR length(fence.family_sha256) <> 64
            OR fence.family_sha256 GLOB '*[^0-9a-f]*'
            OR length(fence.identity_sha256) <> 64
            OR fence.identity_sha256 GLOB '*[^0-9a-f]*'
            OR fence.state NOT IN ('reviewing', 'allowed_pending', 'admitted', 'denied')
            OR fence.created_at_ms < 0
            OR fence.updated_at_ms < fence.created_at_ms
            OR NOT (
                (fence.state = 'denied' AND fence.outcome IN (
                    'guardian_denied',
                    'invalid_decision',
                    'deadline_exceeded',
                    'guardian_error'
                ))
                OR (fence.state <> 'denied' AND fence.outcome IS NULL)
            )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_rows != 0 {
        return Err(StorageError::Message(format!(
            "V54 marker exists but {invalid_rows} permission retry fence row(s) violate authority, identity, or lifecycle constraints"
        )));
    }

    let mut statement = connection.prepare(
        "SELECT root_session_id, authority_history_item_id, review_id
         FROM permission_retry_fences",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (root_session_id, authority_history_item_id, review_id) = row?;
        root_session_id
            .parse::<crate::session::SessionId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V54 permission retry fence has invalid root session id `{root_session_id}`: {error}"
                ))
            })?;
        authority_history_item_id
            .parse::<crate::protocol::HistoryItemId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V54 permission retry fence has invalid authority item id `{authority_history_item_id}`: {error}"
                ))
            })?;
        review_id
            .parse::<crate::storage::PermissionReviewId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V54 permission retry fence has invalid review id `{review_id}`: {error}"
                ))
            })?;
    }
    Ok(())
}

fn validate_durable_side_chats_schema(connection: &Connection) -> Result<(), StorageError> {
    if !schema_migration_applied(connection, DURABLE_SIDE_CHATS_VERSION)? {
        return Err(StorageError::Message(
            "current storage is missing the V55 durable side-chat marker".to_string(),
        ));
    }
    if !schema_migration_has_exact_name(
        connection,
        DURABLE_SIDE_CHATS_VERSION,
        DURABLE_SIDE_CHATS_NAME,
    )? {
        return Err(StorageError::Message(format!(
            "V55 durable side-chat marker has a name other than `{DURABLE_SIDE_CHATS_NAME}`"
        )));
    }
    let expected_connection = canonical_durable_side_chats_connection()?;
    let expected = normalized_schema_objects_for_table(&expected_connection, "side_chat_bindings")?;
    let observed = normalized_schema_objects_for_table(connection, "side_chat_bindings")?;
    for ((object_type, object_name), expected_sql) in &expected {
        let Some(observed_sql) = observed.get(&(object_type.clone(), object_name.clone())) else {
            return Err(StorageError::Message(format!(
                "V55 marker exists but required {object_type} `{object_name}` is missing"
            )));
        };
        if observed_sql != expected_sql {
            return Err(StorageError::Message(format!(
                "V55 marker exists but {object_type} `{object_name}` has stale SQL"
            )));
        }
    }
    for (object_type, object_name) in observed.keys() {
        if !expected.contains_key(&(object_type.clone(), object_name.clone())) {
            return Err(StorageError::Message(format!(
                "V55 marker exists but unexpected {object_type} `{object_name}` owns `side_chat_bindings`"
            )));
        }
    }
    Ok(())
}

fn canonical_durable_side_chats_connection() -> Result<Connection, StorageError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE sessions (
             id TEXT PRIMARY KEY,
             project_id TEXT NOT NULL,
             status TEXT NOT NULL,
             model_name TEXT NOT NULL,
             base_url TEXT NOT NULL,
             active_run_id TEXT,
             active_turn_id TEXT,
             active_run_lease_expires_at_ms INTEGER
         );
         CREATE TABLE session_spawn_edges (
             root_session_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             child_session_id TEXT NOT NULL
         );
         CREATE TABLE moyai_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             name TEXT NOT NULL
         );",
    )?;
    connection.execute_batch(V55_DURABLE_SIDE_CHATS)?;
    Ok(connection)
}

fn validate_durable_side_chats_data(connection: &Connection) -> Result<(), StorageError> {
    let invalid_rows = connection.query_row(
        "SELECT COUNT(*)
         FROM side_chat_bindings AS binding
         LEFT JOIN sessions AS owner ON owner.id = binding.owner_session_id
         LEFT JOIN sessions AS conversation
           ON conversation.id = binding.conversation_session_id
         WHERE owner.id IS NULL
            OR conversation.id IS NULL
            OR binding.owner_session_id = binding.conversation_session_id
            OR owner.project_id <> conversation.project_id
            OR conversation.model_name <> binding.model
            OR conversation.base_url <> binding.base_url
            OR EXISTS (
                SELECT 1 FROM side_chat_bindings AS parent_binding
                WHERE parent_binding.conversation_session_id = binding.owner_session_id
            )
            OR EXISTS (
                SELECT 1 FROM session_spawn_edges AS edge
                WHERE edge.root_session_id = binding.conversation_session_id
                   OR edge.parent_session_id = binding.conversation_session_id
                   OR edge.child_session_id = binding.conversation_session_id
            )
            OR binding.context_scope <> 'general'
            OR binding.context_window <= 0
            OR binding.context_window > 4294967295
            OR binding.max_output_tokens <= 0
            OR binding.max_output_tokens > 4294967295
            OR binding.request_timeout_ms <= 0
            OR binding.connect_timeout_ms <= 0
            OR binding.max_retries < 0
            OR binding.max_retries > 255
            OR binding.supports_images NOT IN (0, 1)
            OR binding.supports_tools NOT IN (0, 1)
            OR binding.supports_reasoning NOT IN (0, 1)
            OR length(CAST(binding.persisted_draft AS BLOB)) > 1048576
            OR binding.draft_revision < 0
            OR binding.request_generation < 0
            OR binding.delete_requested_at_ms < 0
            OR binding.created_at_ms < 0
            OR binding.updated_at_ms < binding.created_at_ms",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if invalid_rows != 0 {
        return Err(StorageError::Message(format!(
            "V55 marker exists but {invalid_rows} side chat binding row(s) violate canonical ownership, provider, or revision constraints"
        )));
    }

    let mut statement = connection.prepare(
        "SELECT id, owner_session_id, conversation_session_id
         FROM side_chat_bindings",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (id, owner_session_id, conversation_session_id) = row?;
        id.parse::<crate::storage::SideChatId>().map_err(|error| {
            StorageError::Message(format!(
                "V55 side chat binding has invalid id `{id}`: {error}"
            ))
        })?;
        owner_session_id
            .parse::<crate::session::SessionId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V55 side chat binding has invalid owner session id `{owner_session_id}`: {error}"
                ))
            })?;
        conversation_session_id
            .parse::<crate::session::SessionId>()
            .map_err(|error| {
                StorageError::Message(format!(
                    "V55 side chat binding has invalid conversation session id `{conversation_session_id}`: {error}"
                ))
            })?;
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
mod tests;
