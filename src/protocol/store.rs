use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::error::StorageError;
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, HistoryScope, ModeKind,
    ModelResponseId, RuntimeEvent, RuntimeEventId, RuntimeEventMsg, SubAgentActivityKind, TurnId,
    TurnInterruptionCause, TurnItem, TurnItemId, TurnItemPayload, project_sub_agent_activity,
};
use crate::runtime::SystemClock;
use crate::session::{AdmissionId, SessionId, SessionSpawnEdge};
use crate::storage::session_repo::{
    SessionProtocolWriteAuthority, fresh_active_admission_matches_in_connection,
    normalize_run_lease_now_ms,
};

pub trait ProtocolEventStore {
    #[cfg(test)]
    fn list_runtime_events(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<RuntimeEvent>, StorageError>;
    #[cfg(test)]
    fn list_runtime_events_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<RuntimeEvent>, StorageError>;
    fn runtime_event_page_for_session(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolPage<RuntimeEvent>, StorageError>;
    fn runtime_event_cursor_page_for_session(
        &self,
        session_id: SessionId,
        after_append_position: Option<i64>,
        limit: usize,
    ) -> Result<ProtocolPage<RuntimeEvent>, StorageError>;
    #[cfg(test)]
    fn list_history_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<HistoryItem>, StorageError>;
    #[cfg(test)]
    fn list_history_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<HistoryItem>, StorageError>;
    fn history_item_page_for_session(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolPage<HistoryItem>, StorageError>;
    fn history_item_cursor_page_for_session(
        &self,
        session_id: SessionId,
        after_append_position: Option<i64>,
        limit: usize,
    ) -> Result<ProtocolPage<HistoryItem>, StorageError>;
    /// Visits the active model-context view in bounded keyset pages under one
    /// immutable storage snapshot. Whole-stream validation and compaction
    /// lineage resolution run once before the first page.
    fn visit_active_history_pages_for_session(
        &self,
        session_id: SessionId,
        limit: usize,
        visitor: &mut dyn FnMut(ActiveHistoryPage) -> Result<(), StorageError>,
    ) -> Result<ActiveHistorySnapshot, StorageError>;
    fn history_items_by_id(
        &self,
        session_id: SessionId,
        item_ids: &[HistoryItemId],
    ) -> Result<Vec<HistoryItem>, StorageError>;
    fn collaboration_mode_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<ModeKind, StorageError>;
    /// Appends a typed mode instruction when `mode` differs from the latest
    /// canonical value. Returns `None` for a same-value update.
    fn set_collaboration_mode(
        &self,
        session_id: SessionId,
        mode: ModeKind,
    ) -> Result<Option<HistoryItem>, StorageError>;
    #[cfg(test)]
    fn list_turn_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<TurnItem>, StorageError>;
    #[cfg(test)]
    fn list_turn_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TurnItem>, StorageError>;
    fn turn_item_page_for_session(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolPage<TurnItem>, StorageError>;
    fn turn_item_cursor_page_for_session(
        &self,
        session_id: SessionId,
        after_append_position: Option<i64>,
        limit: usize,
    ) -> Result<ProtocolPage<TurnItem>, StorageError>;
    fn canonical_snapshot_for_session(
        &self,
        session_id: SessionId,
        history: ProtocolPageRequest,
        turns: ProtocolPageRequest,
    ) -> Result<CanonicalProtocolSnapshot, StorageError>;
    fn latest_turn_position_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(TurnId, i64)>, StorageError>;
    #[cfg(test)]
    fn fork_canonical_items(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<(usize, usize), StorageError>;
    fn fork_agent_context(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<usize, StorageError>;
}

/// One storage page is deliberately small enough to bound SQLite row decoding,
/// model-context hydration, explicit export, and UI snapshots with one contract.
pub const MAX_PROTOCOL_PAGE_LIMIT: usize = 200;

const LATEST_COLLABORATION_MODE_SQL: &str = "SELECT history.payload_json
     FROM protocol_history_items AS history
          INDEXED BY idx_protocol_history_collaboration_mode_session
     CROSS JOIN protocol_item_append_order AS append_order
     WHERE history.session_id = ?1
       AND history.scope_kind = 'session'
       AND history.turn_id IS NULL
       AND json_valid(history.payload_json)
       AND json_extract(history.payload_json, '$.kind') = 'collaboration_mode_instruction'
       AND append_order.session_id = history.session_id
       AND append_order.source_kind = 'history_item'
       AND append_order.source_id = history.id
     ORDER BY append_order.append_position DESC
     LIMIT 1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolPageRequest {
    Offset {
        offset: usize,
        limit: usize,
    },
    Latest {
        limit: usize,
    },
    After {
        append_position: Option<i64>,
        limit: usize,
    },
}

impl ProtocolPageRequest {
    fn resolve(self, total: usize) -> Result<(usize, usize, Option<i64>), StorageError> {
        let (offset, limit, after_append_position) = match self {
            Self::Offset { offset, limit } => (offset, limit, None),
            Self::Latest { limit } => (latest_page_offset(total, limit), limit, None),
            Self::After {
                append_position,
                limit,
            } => (0, limit, append_position),
        };
        if limit == 0 {
            return Err(StorageError::Message(
                "protocol item page limit must be greater than zero".to_string(),
            ));
        }
        if limit > MAX_PROTOCOL_PAGE_LIMIT {
            return Err(StorageError::Message(format!(
                "protocol item page limit {limit} exceeds the maximum {MAX_PROTOCOL_PAGE_LIMIT}"
            )));
        }
        Ok((offset, limit, after_append_position))
    }
}

#[derive(Debug, Clone)]
pub struct ProtocolPage<T> {
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub items: Vec<T>,
    pub next_cursor: Option<i64>,
}

/// One bounded page from a single active-history traversal.
#[derive(Debug, Clone)]
pub struct ActiveHistoryPage {
    pub items: Vec<HistoryItem>,
    pub has_more: bool,
}

/// The immutable source snapshot shared by every page in one traversal.
///
/// Active pages exclude rows hidden by committed compaction. The durable counts
/// still describe the complete canonical stream so terminal admission never
/// mistakes compacted input for unseen input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveHistorySnapshot {
    pub append_fence: Option<i64>,
    pub canonical_count: usize,
    pub steer_count: usize,
    pub agent_communication_count: usize,
    pub active_count: usize,
}

impl<T> ProtocolPage<T> {
    pub fn has_more(&self) -> bool {
        self.offset.saturating_add(self.items.len()) < self.total
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalProtocolFence {
    pub append_position: Option<i64>,
    pub history_count: usize,
    pub turn_count: usize,
    pub runtime_event_count: usize,
}

#[derive(Debug, Clone)]
pub struct CanonicalProtocolSnapshot {
    pub fence: CanonicalProtocolFence,
    pub history: ProtocolPage<HistoryItem>,
    pub turns: ProtocolPage<TurnItem>,
    pub latest_turn_position: Option<(TurnId, i64)>,
}

#[derive(Clone)]
pub struct SqliteProtocolEventStore {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredProtocolEventBundle {
    pub runtime_event: RuntimeEvent,
    pub history_item: Option<HistoryItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentContextForkStats {
    copied_items: usize,
    #[cfg(test)]
    source_pages: usize,
    #[cfg(test)]
    max_source_page_items: usize,
    #[cfg(test)]
    source_fence: ActiveHistorySnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveHistoryCursor {
    effective_position: i64,
    append_position: i64,
}

#[derive(Debug)]
struct ActiveHistoryTraversalPage {
    items: Vec<HistoryItem>,
    next_cursor: Option<ActiveHistoryCursor>,
    has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveHistoryTraversalStats {
    snapshot: ActiveHistorySnapshot,
    pages: usize,
    max_page_items: usize,
}

#[cfg(test)]
const ACTIVE_HISTORY_TRAVERSAL_TABLE: &str = "moyai_active_history_traversal";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CanonicalForkStats {
    copied_history_items: usize,
    copied_turn_items: usize,
    #[cfg(test)]
    history_mapping_pages: usize,
    #[cfg(test)]
    history_copy_pages: usize,
    #[cfg(test)]
    turn_copy_pages: usize,
    #[cfg(test)]
    max_source_page_items: usize,
    #[cfg(test)]
    source_fence: CanonicalProtocolFence,
}

/// Bounded durable state needed to restore or project one retained direct child.
///
/// This deliberately contains only point/latest protocol projections. Callers never need to
/// materialize the child's complete canonical history or runtime-event stream.
#[derive(Debug, Clone)]
pub(crate) struct RetainedDirectChildProjection {
    pub edge: SessionSpawnEdge,
    pub session_status: String,
    pub latest_task_content: Option<Vec<ContentPart>>,
    pub latest_assistant_content: Option<Vec<ContentPart>>,
    pub latest_error: Option<String>,
    pub interruption_cause: Option<TurnInterruptionCause>,
}

#[derive(Debug, Clone)]
pub(crate) struct DurableChildResultProjection {
    pub latest_assistant_content: Option<Vec<ContentPart>>,
    pub latest_error: Option<String>,
}

impl SqliteProtocolEventStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    pub(crate) fn retained_direct_child_page(
        &self,
        root_session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolPage<RetainedDirectChildProjection>, StorageError> {
        if limit == 0 || limit > MAX_PROTOCOL_PAGE_LIMIT {
            return Err(StorageError::Message(format!(
                "retained child page limit must be between 1 and {MAX_PROTOCOL_PAGE_LIMIT}, got {limit}"
            )));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total = transaction.query_row(
            "SELECT COUNT(*) FROM session_spawn_edges WHERE root_session_id = ?1",
            params![root_session_id.to_string()],
            |row| row.get::<_, i64>(0),
        )?;
        let total = usize::try_from(total).map_err(|_| {
            StorageError::Message(format!(
                "retained child count for session {root_session_id} exceeds this platform's range"
            ))
        })?;
        let mut statement = transaction.prepare(
            "SELECT edge.root_session_id, edge.parent_session_id, edge.child_session_id,
                    edge.agent_path, edge.task_name, edge.created_at_ms, child.status,
                    (
                        SELECT history.payload_json
                        FROM protocol_history_items AS history
                        INNER JOIN protocol_item_append_order AS append_order
                          ON append_order.session_id = history.session_id
                         AND append_order.source_kind = 'history_item'
                         AND append_order.source_id = history.id
                        WHERE history.session_id = edge.child_session_id
                          AND json_extract(history.payload_json, '$.kind') IN ('user_turn', 'steer_turn')
                        ORDER BY append_order.append_position DESC
                        LIMIT 1
                    ),
                    (
                        SELECT history.payload_json
                        FROM protocol_history_items AS history
                        INNER JOIN protocol_item_append_order AS append_order
                          ON append_order.session_id = history.session_id
                         AND append_order.source_kind = 'history_item'
                         AND append_order.source_id = history.id
                        WHERE history.session_id = edge.child_session_id
                          AND json_extract(history.payload_json, '$.kind') = 'assistant_message'
                        ORDER BY append_order.append_position DESC
                        LIMIT 1
                    ),
                    (
                        SELECT history.payload_json
                        FROM protocol_history_items AS history
                        INNER JOIN protocol_item_append_order AS append_order
                          ON append_order.session_id = history.session_id
                         AND append_order.source_kind = 'history_item'
                         AND append_order.source_id = history.id
                        WHERE history.session_id = edge.child_session_id
                          AND json_extract(history.payload_json, '$.kind') = 'error'
                        ORDER BY append_order.append_position DESC
                        LIMIT 1
                    ),
                    (
                        SELECT runtime_event.msg_json
                        FROM protocol_runtime_events AS runtime_event
                        INNER JOIN protocol_item_append_order AS append_order
                          ON append_order.session_id = runtime_event.session_id
                         AND append_order.source_kind = 'runtime_event'
                         AND append_order.source_id = runtime_event.id
                        WHERE runtime_event.session_id = edge.child_session_id
                          AND json_extract(runtime_event.msg_json, '$.kind') = 'turn_terminal'
                        ORDER BY append_order.append_position DESC
                        LIMIT 1
                    )
             FROM session_spawn_edges AS edge
             INNER JOIN sessions AS child ON child.id = edge.child_session_id
             WHERE edge.root_session_id = ?1
             ORDER BY edge.created_at_ms ASC, edge.child_session_id ASC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(
            params![
                root_session_id.to_string(),
                sqlite_page_value(limit),
                sqlite_page_value(offset)
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in rows {
            let (
                edge_root,
                parent,
                child,
                agent_path,
                task_name,
                created_at_ms,
                session_status,
                task_payload,
                assistant_payload,
                error_payload,
                terminal_payload,
            ) = row?;
            items.push(RetainedDirectChildProjection {
                edge: SessionSpawnEdge {
                    root_session_id: parse_session_id(&edge_root, "retained child root")?,
                    parent_session_id: parse_session_id(&parent, "retained child parent")?,
                    child_session_id: parse_session_id(&child, "retained child session")?,
                    agent_path,
                    task_name,
                    created_at_ms,
                },
                session_status,
                latest_task_content: optional_input_content(task_payload)?,
                latest_assistant_content: optional_assistant_content(assistant_payload)?,
                latest_error: optional_error_message(error_payload)?,
                interruption_cause: optional_terminal_interruption_cause(terminal_payload)?,
            });
        }
        drop(statement);
        transaction.commit()?;
        Ok(ProtocolPage {
            offset,
            limit,
            total,
            items,
            next_cursor: None,
        })
    }

    pub(crate) fn assistant_content_for_response(
        &self,
        session_id: SessionId,
        response_id: ModelResponseId,
    ) -> Result<Option<Vec<ContentPart>>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let payload = connection
            .query_row(
                "SELECT history.payload_json
                 FROM protocol_history_items AS history
                 INNER JOIN protocol_item_append_order AS append_order
                   ON append_order.session_id = history.session_id
                  AND append_order.source_kind = 'history_item'
                  AND append_order.source_id = history.id
                 WHERE history.session_id = ?1
                   AND json_extract(history.payload_json, '$.kind') = 'assistant_message'
                   AND json_extract(history.payload_json, '$.response_id') = ?2
                 ORDER BY append_order.append_position DESC
                 LIMIT 1",
                params![session_id.to_string(), response_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        optional_assistant_content(payload)
    }

    pub(crate) fn durable_child_result_projection(
        &self,
        session_id: SessionId,
    ) -> Result<DurableChildResultProjection, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let (assistant_payload, error_payload) = connection.query_row(
            "SELECT
                 (
                     SELECT history.payload_json
                     FROM protocol_history_items AS history
                     INNER JOIN protocol_item_append_order AS append_order
                       ON append_order.session_id = history.session_id
                      AND append_order.source_kind = 'history_item'
                      AND append_order.source_id = history.id
                     WHERE history.session_id = ?1
                       AND json_extract(history.payload_json, '$.kind') = 'assistant_message'
                     ORDER BY append_order.append_position DESC
                     LIMIT 1
                 ),
                 (
                     SELECT history.payload_json
                     FROM protocol_history_items AS history
                     INNER JOIN protocol_item_append_order AS append_order
                       ON append_order.session_id = history.session_id
                      AND append_order.source_kind = 'history_item'
                      AND append_order.source_id = history.id
                     WHERE history.session_id = ?1
                       AND json_extract(history.payload_json, '$.kind') = 'error'
                     ORDER BY append_order.append_position DESC
                     LIMIT 1
                 )",
            params![session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        Ok(DurableChildResultProjection {
            latest_assistant_content: optional_assistant_content(assistant_payload)?,
            latest_error: optional_error_message(error_payload)?,
        })
    }

    /// Captures the latest bounded runtime-event page and its append fence in one
    /// read transaction. Runtime subscriptions use this tail snapshot instead of
    /// materializing an unbounded session lifetime.
    pub fn latest_runtime_event_page_for_session(
        &self,
        session_id: SessionId,
        limit: usize,
    ) -> Result<ProtocolPage<RuntimeEvent>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total = protocol_source_count(
            &transaction,
            session_id,
            "protocol_runtime_events",
            "runtime_event",
        )?;
        let page = runtime_event_page_from_connection(
            &transaction,
            session_id,
            ProtocolPageRequest::Latest { limit },
            total,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    pub fn runtime_event_page_for_turn_after_sequence(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        after_sequence_no: Option<i64>,
        limit: usize,
    ) -> Result<Vec<RuntimeEvent>, StorageError> {
        if limit == 0 || limit > MAX_PROTOCOL_PAGE_LIMIT {
            return Err(StorageError::Message(format!(
                "runtime-event turn page limit must be between 1 and {MAX_PROTOCOL_PAGE_LIMIT}, got {limit}"
            )));
        }
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, sequence_no, msg_json, created_at_ms
             FROM protocol_runtime_events
             WHERE session_id = ?1 AND turn_id = ?2
               AND (?3 IS NULL OR sequence_no > ?3)
             ORDER BY sequence_no ASC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                session_id.to_string(),
                turn_id.to_string(),
                after_sequence_no,
                limit as i64,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut events = Vec::with_capacity(limit);
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

    pub(super) fn append_recording_projection_allocating(
        &self,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<StoredProtocolEventBundle, StorageError> {
        validate_recording_projection(event, history_item, turn_item)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = insert_event_bundle_unchecked(&transaction, event, history_item, turn_item)?;
        transaction.commit()?;
        Ok(stored)
    }

    pub(super) fn append_admitted_recording_projection_allocating(
        &self,
        admission_id: crate::session::AdmissionId,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<Option<StoredProtocolEventBundle>, StorageError> {
        self.append_admitted_recording_projection_allocating_at(
            admission_id,
            event,
            history_item,
            turn_item,
            SystemClock::now_ms(),
        )
    }

    pub(super) fn append_admitted_recording_projection_allocating_at(
        &self,
        admission_id: crate::session::AdmissionId,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
        now_ms: i64,
    ) -> Result<Option<StoredProtocolEventBundle>, StorageError> {
        validate_recording_projection(event, history_item, turn_item)?;
        let now = normalize_run_lease_now_ms(now_ms);
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned = fresh_active_admission_matches_in_connection(
            &transaction,
            event.session_id,
            admission_id,
            event.turn_id,
            now,
        )?;
        if !owned {
            transaction.commit()?;
            return Ok(None);
        }
        let stored = insert_event_bundle_unchecked(&transaction, event, history_item, turn_item)?;
        transaction.commit()?;
        Ok(Some(stored))
    }

    /// Records the runtime/control projection owned by the multi-agent runtime.
    ///
    /// Unlike the recording sink this API cannot accept an arbitrary projection,
    /// so model-response, tool-settlement, and terminal payloads remain reachable
    /// only through their session-repository transactions.
    pub(crate) fn append_sub_agent_activity(
        &self,
        root_session_id: SessionId,
        originating_admission_id: AdmissionId,
        originating_turn_id: TurnId,
        activity_id: String,
        agent_session_id: SessionId,
        agent_path: String,
        activity_kind: SubAgentActivityKind,
    ) -> Result<(), StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owns_root_turn = fresh_active_admission_matches_in_connection(
            &transaction,
            root_session_id,
            originating_admission_id,
            originating_turn_id,
            now,
        )?;
        let owned_lineage = owns_root_turn
            && transaction
                .query_row(
                    "SELECT 1
                     FROM session_spawn_edges AS edge
                     WHERE edge.root_session_id = ?1
                       AND edge.parent_session_id = ?1
                       AND edge.child_session_id = ?2
                       AND edge.agent_path = ?3",
                    params![
                        root_session_id.to_string(),
                        agent_session_id.to_string(),
                        agent_path
                    ],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
        if !owned_lineage {
            return Err(StorageError::Message(format!(
                "sub-agent activity owner or retained direct-child identity is stale for root session {root_session_id} admission {originating_admission_id} turn {originating_turn_id}"
            )));
        }
        let projection = project_sub_agent_activity(
            root_session_id,
            originating_turn_id,
            0,
            activity_id,
            agent_session_id,
            agent_path,
            activity_kind,
        );
        insert_event_bundle_unchecked(
            &transaction,
            &projection.runtime_event,
            projection.history_item.as_ref(),
            projection.turn_item.as_ref(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seed_runtime_event_for_test(
        &self,
        event: &RuntimeEvent,
    ) -> Result<(), StorageError> {
        self.seed_event_bundle_for_test(event, None, None)
    }

    #[cfg(test)]
    pub(crate) fn seed_event_bundle_for_test(
        &self,
        event: &RuntimeEvent,
        history_item: Option<&HistoryItem>,
        turn_item: Option<&TurnItem>,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_event_bundle_unchecked(&transaction, event, history_item, turn_item)?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seed_history_item_for_test(
        &self,
        item: &HistoryItem,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence_no = match item.scope {
            HistoryScope::Turn { turn_id } => claim_protocol_sequence_in_transaction(
                &transaction,
                item.session_id,
                turn_id,
                item.sequence_no,
            )?,
            HistoryScope::Session => claim_session_history_sequence_in_transaction(
                &transaction,
                item.session_id,
                item.sequence_no,
            )?,
        };
        let mut stored_item = item.clone();
        stored_item.sequence_no = sequence_no;
        insert_history_item(&transaction, &stored_item)?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seed_turn_item_for_test(&self, item: &TurnItem) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence_no = claim_protocol_sequence_in_transaction(
            &transaction,
            item.session_id,
            item.turn_id,
            item.sequence_no,
        )?;
        let mut stored_item = item.clone();
        stored_item.sequence_no = sequence_no;
        insert_turn_item(&transaction, &stored_item)?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn fork_agent_context_with_stats_for_test(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
        expected_fence: Option<ActiveHistorySnapshot>,
    ) -> Result<AgentContextForkStats, StorageError> {
        if source_session_id == target_session_id {
            return Err(StorageError::Message(
                "cannot fork agent context into the same session".to_string(),
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stats = fork_agent_context_in_transaction(
            &transaction,
            source_session_id,
            target_session_id,
            expected_fence,
        )?;
        transaction.commit()?;
        Ok(stats)
    }

    #[cfg(test)]
    fn fork_canonical_items_with_stats_for_test(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<CanonicalForkStats, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stats = fork_canonical_items_with_stats_in_transaction(
            &transaction,
            source_session_id,
            target_session_id,
        )?;
        transaction.commit()?;
        Ok(stats)
    }
}

impl ProtocolEventStore for SqliteProtocolEventStore {
    #[cfg(test)]
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

    #[cfg(test)]
    fn list_runtime_events_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<RuntimeEvent>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let total = protocol_source_count(
            &connection,
            session_id,
            "protocol_runtime_events",
            "runtime_event",
        )?;
        let mut statement = connection.prepare(
            "SELECT runtime_event.id, runtime_event.turn_id, runtime_event.sequence_no,
                    runtime_event.msg_json, runtime_event.created_at_ms
             FROM protocol_runtime_events AS runtime_event
             INNER JOIN protocol_item_append_order AS append_order
               ON append_order.session_id = runtime_event.session_id
              AND append_order.source_kind = 'runtime_event'
              AND append_order.source_id = runtime_event.id
             WHERE runtime_event.session_id = ?1
             ORDER BY append_order.append_position ASC",
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
        let mut events = Vec::with_capacity(total);
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

    fn runtime_event_page_for_session(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolPage<RuntimeEvent>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total = protocol_source_count(
            &transaction,
            session_id,
            "protocol_runtime_events",
            "runtime_event",
        )?;
        let page = runtime_event_page_from_connection(
            &transaction,
            session_id,
            ProtocolPageRequest::Offset { offset, limit },
            total,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    fn runtime_event_cursor_page_for_session(
        &self,
        session_id: SessionId,
        after_append_position: Option<i64>,
        limit: usize,
    ) -> Result<ProtocolPage<RuntimeEvent>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total = protocol_source_count(
            &transaction,
            session_id,
            "protocol_runtime_events",
            "runtime_event",
        )?;
        let page = runtime_event_page_from_connection(
            &transaction,
            session_id,
            ProtocolPageRequest::After {
                append_position: after_append_position,
                limit,
            },
            total,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    #[cfg(test)]
    fn list_history_items(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<HistoryItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, sequence_no, payload_json, created_at_ms
             FROM protocol_history_items
             WHERE session_id = ?1 AND scope_kind = 'turn' AND turn_id = ?2
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
                scope: HistoryScope::Turn { turn_id },
                sequence_no,
                created_at_ms,
                payload: serde_json::from_str::<HistoryItemPayload>(&payload_json)?,
            });
        }
        Ok(items)
    }

    #[cfg(test)]
    fn list_history_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<HistoryItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        list_history_items_for_session_from_connection(&connection, session_id)
    }

    fn history_item_page_for_session(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolPage<HistoryItem>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total = protocol_source_count(
            &transaction,
            session_id,
            "protocol_history_items",
            "history_item",
        )?;
        let page = history_item_page_from_connection(
            &transaction,
            session_id,
            ProtocolPageRequest::Offset { offset, limit },
            total,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    fn history_item_cursor_page_for_session(
        &self,
        session_id: SessionId,
        after_append_position: Option<i64>,
        limit: usize,
    ) -> Result<ProtocolPage<HistoryItem>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total = protocol_source_count(
            &transaction,
            session_id,
            "protocol_history_items",
            "history_item",
        )?;
        let page = history_item_page_from_connection(
            &transaction,
            session_id,
            ProtocolPageRequest::After {
                append_position: after_append_position,
                limit,
            },
            total,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    fn visit_active_history_pages_for_session(
        &self,
        session_id: SessionId,
        limit: usize,
        visitor: &mut dyn FnMut(ActiveHistoryPage) -> Result<(), StorageError>,
    ) -> Result<ActiveHistorySnapshot, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let stats =
            traverse_active_history_in_transaction(&transaction, session_id, None, limit, visitor)?;
        transaction.commit()?;
        Ok(stats.snapshot)
    }

    fn history_items_by_id(
        &self,
        session_id: SessionId,
        item_ids: &[HistoryItemId],
    ) -> Result<Vec<HistoryItem>, StorageError> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        if item_ids.len() > MAX_PROTOCOL_PAGE_LIMIT {
            return Err(StorageError::Message(format!(
                "history item identity query count {} exceeds the maximum {MAX_PROTOCOL_PAGE_LIMIT}",
                item_ids.len()
            )));
        }
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        history_items_by_id_from_connection(&connection, session_id, item_ids)
    }

    fn collaboration_mode_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<ModeKind, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        latest_collaboration_mode_from_connection(&connection, session_id)
    }

    fn set_collaboration_mode(
        &self,
        session_id: SessionId,
        mode: ModeKind,
    ) -> Result<Option<HistoryItem>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if latest_collaboration_mode_from_connection(&transaction, session_id)? == mode {
            transaction.commit()?;
            return Ok(None);
        }
        let item = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Session,
            sequence_no: claim_session_history_sequence_in_transaction(
                &transaction,
                session_id,
                0,
            )?,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::CollaborationModeInstruction { mode },
        };
        insert_history_item(&transaction, &item)?;
        transaction.commit()?;
        Ok(Some(item))
    }

    #[cfg(test)]
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

    #[cfg(test)]
    fn list_turn_items_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TurnItem>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        list_turn_items_for_session_from_connection(&connection, session_id)
    }

    fn turn_item_page_for_session(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolPage<TurnItem>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total =
            protocol_source_count(&transaction, session_id, "protocol_turn_items", "turn_item")?;
        let page = turn_item_page_from_connection(
            &transaction,
            session_id,
            ProtocolPageRequest::Offset { offset, limit },
            total,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    fn turn_item_cursor_page_for_session(
        &self,
        session_id: SessionId,
        after_append_position: Option<i64>,
        limit: usize,
    ) -> Result<ProtocolPage<TurnItem>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let total =
            protocol_source_count(&transaction, session_id, "protocol_turn_items", "turn_item")?;
        let page = turn_item_page_from_connection(
            &transaction,
            session_id,
            ProtocolPageRequest::After {
                append_position: after_append_position,
                limit,
            },
            total,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    fn canonical_snapshot_for_session(
        &self,
        session_id: SessionId,
        history: ProtocolPageRequest,
        turns: ProtocolPageRequest,
    ) -> Result<CanonicalProtocolSnapshot, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let snapshot =
            canonical_protocol_snapshot_from_connection(&transaction, session_id, history, turns)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    fn latest_turn_position_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(TurnId, i64)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        latest_turn_position_for_session(&*connection, session_id)
    }

    #[cfg(test)]
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
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let copied = fork_canonical_items_in_transaction(
            &transaction,
            source_session_id,
            target_session_id,
        )?;
        transaction.commit()?;
        Ok(copied)
    }

    fn fork_agent_context(
        &self,
        source_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Result<usize, StorageError> {
        if source_session_id == target_session_id {
            return Err(StorageError::Message(
                "cannot fork agent context into the same session".to_string(),
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stats = fork_agent_context_in_transaction(
            &transaction,
            source_session_id,
            target_session_id,
            None,
        )?;
        transaction.commit()?;
        Ok(stats.copied_items)
    }
}

fn fork_agent_context_in_transaction(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
    expected_fence: Option<ActiveHistorySnapshot>,
) -> Result<AgentContextForkStats, StorageError> {
    ensure_empty_protocol_target(transaction, target_session_id, "agent context")?;

    let mut copied_items = 0usize;
    let mut copy_page = |source_page: ActiveHistoryPage| {
        let source_item_count = source_page.items.len();
        let mut forked_page = Vec::with_capacity(source_item_count);
        for item in source_page.items {
            let Some(payload) = fork_agent_context_payload(item.payload) else {
                continue;
            };
            forked_page.push(HistoryItem {
                id: HistoryItemId::new(),
                session_id: target_session_id,
                payload,
                ..item
            });
        }
        for item in &forked_page {
            insert_history_item(transaction, item)?;
        }
        seed_history_turn_sequence_allocators(transaction, target_session_id, &forked_page)?;
        copied_items = copied_items.saturating_add(forked_page.len());
        Ok(())
    };
    let _traversal = traverse_active_history_in_transaction(
        transaction,
        source_session_id,
        expected_fence,
        MAX_PROTOCOL_PAGE_LIMIT,
        &mut copy_page,
    )?;
    Ok(AgentContextForkStats {
        copied_items,
        #[cfg(test)]
        source_pages: _traversal.pages,
        #[cfg(test)]
        max_source_page_items: _traversal.max_page_items,
        #[cfg(test)]
        source_fence: _traversal.snapshot,
    })
}

pub(crate) fn fork_canonical_items_in_transaction(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
) -> Result<(usize, usize), StorageError> {
    let stats = fork_canonical_items_with_stats_in_transaction(
        transaction,
        source_session_id,
        target_session_id,
    )?;
    Ok((stats.copied_history_items, stats.copied_turn_items))
}

#[cfg(test)]
const CANONICAL_FORK_HISTORY_ID_MAP_TABLE: &str = "moyai_canonical_fork_history_id_map";

fn fork_canonical_items_with_stats_in_transaction(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
) -> Result<CanonicalForkStats, StorageError> {
    if source_session_id == target_session_id {
        return Err(StorageError::Message(
            "cannot fork canonical items into the same session".to_string(),
        ));
    }
    ensure_protocol_source_exists(transaction, source_session_id, "canonical items")?;
    ensure_empty_protocol_target(transaction, target_session_id, "canonical items")?;
    let source_fence = canonical_protocol_fence_from_connection(transaction, source_session_id)?;

    with_canonical_fork_history_id_map(transaction, || {
        let mut history_mapping_offset = 0usize;
        #[cfg(test)]
        let mut history_mapping_pages = 0usize;
        #[cfg(test)]
        let mut history_copy_pages = 0usize;
        #[cfg(test)]
        let mut turn_copy_pages = 0usize;
        #[cfg(test)]
        let mut max_source_page_items = 0usize;

        while history_mapping_offset < source_fence.history_count {
            let source_page = history_item_page_from_connection(
                transaction,
                source_session_id,
                ProtocolPageRequest::Offset {
                    offset: history_mapping_offset,
                    limit: MAX_PROTOCOL_PAGE_LIMIT,
                },
                source_fence.history_count,
            )?;
            let source_page_items = source_page.items.len();
            #[cfg(test)]
            {
                history_mapping_pages = history_mapping_pages.saturating_add(1);
                max_source_page_items = max_source_page_items.max(source_page_items);
            }
            for item in source_page.items {
                insert_canonical_fork_history_id_mapping(
                    transaction,
                    source_session_id,
                    target_session_id,
                    item.id,
                    HistoryItemId::new(),
                )?;
            }
            history_mapping_offset = advance_canonical_fork_page_offset(
                source_session_id,
                "history identity mapping",
                history_mapping_offset,
                source_page_items,
                source_fence.history_count,
            )?;
        }
        ensure_canonical_fork_history_mapping_count(
            transaction,
            source_session_id,
            target_session_id,
            source_fence.history_count,
        )?;

        let mut history_copy_offset = 0usize;
        let mut copied_history_items = 0usize;
        while history_copy_offset < source_fence.history_count {
            let source_page = history_item_page_from_connection(
                transaction,
                source_session_id,
                ProtocolPageRequest::Offset {
                    offset: history_copy_offset,
                    limit: MAX_PROTOCOL_PAGE_LIMIT,
                },
                source_fence.history_count,
            )?;
            let source_page_items = source_page.items.len();
            #[cfg(test)]
            {
                history_copy_pages = history_copy_pages.saturating_add(1);
                max_source_page_items = max_source_page_items.max(source_page_items);
            }
            let mut forked_page = Vec::with_capacity(source_page_items);
            for item in source_page.items {
                let target_id = canonical_fork_target_history_id(
                    transaction,
                    source_session_id,
                    target_session_id,
                    item.id,
                )?;
                let payload = fork_history_payload_for_session(
                    transaction,
                    source_session_id,
                    target_session_id,
                    item.payload,
                )?;
                forked_page.push(HistoryItem {
                    id: target_id,
                    session_id: target_session_id,
                    payload,
                    ..item
                });
            }
            for item in &forked_page {
                insert_history_item(transaction, item)?;
            }
            seed_history_turn_sequence_allocators(transaction, target_session_id, &forked_page)?;
            copied_history_items = copied_history_items.saturating_add(forked_page.len());
            history_copy_offset = advance_canonical_fork_page_offset(
                source_session_id,
                "history copy",
                history_copy_offset,
                source_page_items,
                source_fence.history_count,
            )?;
        }

        let mut turn_copy_offset = 0usize;
        let mut copied_turn_items = 0usize;
        while turn_copy_offset < source_fence.turn_count {
            let source_page = turn_item_page_from_connection(
                transaction,
                source_session_id,
                ProtocolPageRequest::Offset {
                    offset: turn_copy_offset,
                    limit: MAX_PROTOCOL_PAGE_LIMIT,
                },
                source_fence.turn_count,
            )?;
            let source_page_items = source_page.items.len();
            #[cfg(test)]
            {
                turn_copy_pages = turn_copy_pages.saturating_add(1);
                max_source_page_items = max_source_page_items.max(source_page_items);
            }
            let mut forked_page = Vec::with_capacity(source_page_items);
            for item in source_page.items {
                let source_item_id = item
                    .source_item_id
                    .map(|source_id| {
                        canonical_fork_target_history_id(
                            transaction,
                            source_session_id,
                            target_session_id,
                            source_id,
                        )
                    })
                    .transpose()?;
                forked_page.push(TurnItem {
                    id: TurnItemId::new(),
                    session_id: target_session_id,
                    source_item_id,
                    ..item
                });
            }
            for item in &forked_page {
                insert_turn_item(transaction, item)?;
            }
            seed_turn_sequence_allocators(
                transaction,
                target_session_id,
                forked_page
                    .iter()
                    .map(|item| (item.turn_id, item.sequence_no)),
            )?;
            copied_turn_items = copied_turn_items.saturating_add(forked_page.len());
            turn_copy_offset = advance_canonical_fork_page_offset(
                source_session_id,
                "turn copy",
                turn_copy_offset,
                source_page_items,
                source_fence.turn_count,
            )?;
        }

        let final_fence = canonical_protocol_fence_from_connection(transaction, source_session_id)?;
        ensure_canonical_fork_source_fence(source_session_id, source_fence, final_fence)?;
        Ok(CanonicalForkStats {
            copied_history_items,
            copied_turn_items,
            #[cfg(test)]
            history_mapping_pages,
            #[cfg(test)]
            history_copy_pages,
            #[cfg(test)]
            turn_copy_pages,
            #[cfg(test)]
            max_source_page_items,
            #[cfg(test)]
            source_fence,
        })
    })
}

fn with_canonical_fork_history_id_map<T>(
    transaction: &Transaction<'_>,
    operation: impl FnOnce() -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    transaction.execute_batch(
        "DROP TABLE IF EXISTS temp.moyai_canonical_fork_history_id_map;
         CREATE TEMP TABLE temp.moyai_canonical_fork_history_id_map (
             source_session_id TEXT NOT NULL,
             target_session_id TEXT NOT NULL,
             source_id TEXT NOT NULL,
             target_id TEXT NOT NULL,
             PRIMARY KEY (source_session_id, target_session_id, source_id),
             UNIQUE (target_session_id, target_id)
         ) WITHOUT ROWID;",
    )?;
    let result = operation();
    let cleanup =
        transaction.execute_batch("DROP TABLE IF EXISTS temp.moyai_canonical_fork_history_id_map;");
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(StorageError::from(error)),
    }
}

fn insert_canonical_fork_history_id_mapping(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
    source_id: HistoryItemId,
    target_id: HistoryItemId,
) -> Result<(), StorageError> {
    transaction.execute(
        "INSERT INTO temp.moyai_canonical_fork_history_id_map
             (source_session_id, target_session_id, source_id, target_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            source_session_id.to_string(),
            target_session_id.to_string(),
            source_id.to_string(),
            target_id.to_string()
        ],
    )?;
    Ok(())
}

fn canonical_fork_target_history_id(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
    source_id: HistoryItemId,
) -> Result<HistoryItemId, StorageError> {
    let target_id = transaction
        .query_row(
            "SELECT target_id
             FROM temp.moyai_canonical_fork_history_id_map
             WHERE source_session_id = ?1
               AND target_session_id = ?2
               AND source_id = ?3",
            params![
                source_session_id.to_string(),
                target_session_id.to_string(),
                source_id.to_string()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(target_id) = target_id else {
        return Err(StorageError::Message(format!(
            "canonical fork history mapping for source session {source_session_id}, target session {target_session_id}, and item {source_id} is missing"
        )));
    };
    parse_protocol_id::<HistoryItemId>(&target_id, "canonical fork history mapping")
}

fn ensure_canonical_fork_history_mapping_count(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
    expected_count: usize,
) -> Result<(), StorageError> {
    let actual_count = transaction.query_row(
        "SELECT COUNT(*)
         FROM temp.moyai_canonical_fork_history_id_map
         WHERE source_session_id = ?1 AND target_session_id = ?2",
        params![source_session_id.to_string(), target_session_id.to_string()],
        |row| row.get::<_, i64>(0),
    )?;
    let actual_count = usize::try_from(actual_count).map_err(|_| {
        StorageError::Message(
            "canonical fork history mapping count exceeds this platform's range".to_string(),
        )
    })?;
    if actual_count == expected_count {
        return Ok(());
    }
    Err(StorageError::Message(format!(
        "canonical fork history mapping for source session {source_session_id} and target session {target_session_id} contains {actual_count} rows; expected {expected_count}"
    )))
}

fn advance_canonical_fork_page_offset(
    source_session_id: SessionId,
    page_label: &str,
    offset: usize,
    page_items: usize,
    expected_total: usize,
) -> Result<usize, StorageError> {
    if page_items == 0 {
        return Err(StorageError::Message(format!(
            "canonical fork {page_label} for source session {source_session_id} made no progress at offset {offset} of {expected_total}"
        )));
    }
    let next_offset = offset.saturating_add(page_items);
    if next_offset > expected_total {
        return Err(StorageError::Message(format!(
            "canonical fork {page_label} for source session {source_session_id} advanced to offset {next_offset} beyond {expected_total}"
        )));
    }
    Ok(next_offset)
}

fn canonical_protocol_fence_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<CanonicalProtocolFence, StorageError> {
    let history_count = protocol_source_count(
        connection,
        session_id,
        "protocol_history_items",
        "history_item",
    )?;
    let turn_count =
        protocol_source_count(connection, session_id, "protocol_turn_items", "turn_item")?;
    let runtime_event_count = protocol_source_count(
        connection,
        session_id,
        "protocol_runtime_events",
        "runtime_event",
    )?;
    let append_position = connection.query_row(
        "SELECT MAX(append_position)
         FROM protocol_item_append_order
         WHERE session_id = ?1",
        params![session_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(CanonicalProtocolFence {
        append_position,
        history_count,
        turn_count,
        runtime_event_count,
    })
}

fn ensure_canonical_fork_source_fence(
    source_session_id: SessionId,
    expected: CanonicalProtocolFence,
    actual: CanonicalProtocolFence,
) -> Result<(), StorageError> {
    if expected == actual {
        return Ok(());
    }
    Err(StorageError::Message(format!(
        "canonical protocol source fence changed for session {source_session_id}: expected {expected:?}; observed {actual:?}"
    )))
}

fn ensure_active_history_snapshot(
    session_id: SessionId,
    expected: ActiveHistorySnapshot,
    actual: ActiveHistorySnapshot,
) -> Result<(), StorageError> {
    if expected == actual {
        return Ok(());
    }
    Err(StorageError::CanonicalHistoryFenceChanged {
        session_id,
        expected_append_position: expected.append_fence,
        actual_append_position: actual.append_fence,
        expected_history_count: expected.canonical_count,
        actual_history_count: actual.canonical_count,
        expected_active_count: expected.active_count,
        actual_active_count: actual.active_count,
    })
}

fn ensure_empty_protocol_target(
    transaction: &Transaction<'_>,
    target_session_id: SessionId,
    fork_label: &str,
) -> Result<(), StorageError> {
    let target_session_exists = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
        params![target_session_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if !target_session_exists {
        return Err(StorageError::Message(format!(
            "cannot fork {fork_label} into missing target session {target_session_id}"
        )));
    }
    let target_has_protocol_data = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM protocol_runtime_events WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_history_items WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_turn_items WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_item_append_order WHERE session_id = ?1
             UNION ALL
             SELECT 1 FROM protocol_turn_sequence_allocators WHERE session_id = ?1
         )",
        params![target_session_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if target_has_protocol_data {
        return Err(StorageError::Message(format!(
            "cannot fork {fork_label} into non-empty target session {target_session_id}"
        )));
    }
    Ok(())
}

fn ensure_protocol_source_exists(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    fork_label: &str,
) -> Result<(), StorageError> {
    let source_session_exists = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
        params![source_session_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if source_session_exists {
        return Ok(());
    }
    Err(StorageError::Message(format!(
        "cannot fork {fork_label} from missing source session {source_session_id}"
    )))
}

fn fork_agent_context_payload(payload: HistoryItemPayload) -> Option<HistoryItemPayload> {
    match payload {
        HistoryItemPayload::UserTurn { content, .. } => Some(HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch: None,
            editor_context: None,
        }),
        HistoryItemPayload::AssistantMessage {
            response_id,
            content,
        } => Some(HistoryItemPayload::AssistantMessage {
            response_id,
            content,
        }),
        HistoryItemPayload::CollaborationModeInstruction { mode } => {
            Some(HistoryItemPayload::CollaborationModeInstruction { mode })
        }
        HistoryItemPayload::Compaction { mode, summary, .. } => {
            // The child receives the parent's current semantic summary, not the
            // replaced raw history. Empty replacement lineage makes the copied
            // summary a standalone active context item without inventing an
            // assistant response or retaining inactive parent details.
            Some(HistoryItemPayload::Compaction {
                mode,
                summary,
                replacement_item_ids: Vec::new(),
            })
        }
        _ => None,
    }
}

fn latest_collaboration_mode_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<ModeKind, StorageError> {
    let payload_json = connection
        .query_row(
            LATEST_COLLABORATION_MODE_SQL,
            params![session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(payload_json) = payload_json else {
        return Ok(ModeKind::default());
    };
    match serde_json::from_str::<HistoryItemPayload>(&payload_json)? {
        HistoryItemPayload::CollaborationModeInstruction { mode } => Ok(mode),
        _ => Err(StorageError::Message(format!(
            "indexed collaboration-mode lookup returned a non-mode item for session {session_id}"
        ))),
    }
}

fn seed_history_turn_sequence_allocators(
    transaction: &Transaction<'_>,
    target_session_id: SessionId,
    history_items: &[HistoryItem],
) -> Result<(), StorageError> {
    seed_turn_sequence_allocators(
        transaction,
        target_session_id,
        history_items
            .iter()
            .filter_map(|item| item.turn_id().map(|turn_id| (turn_id, item.sequence_no))),
    )
}

fn seed_turn_sequence_allocators(
    transaction: &Transaction<'_>,
    target_session_id: SessionId,
    turn_sequences: impl IntoIterator<Item = (TurnId, i64)>,
) -> Result<(), StorageError> {
    let mut next_sequence_by_turn = HashMap::<TurnId, i64>::new();
    for (turn_id, sequence_no) in turn_sequences {
        let next_sequence_no = sequence_no.max(-1).saturating_add(1);
        next_sequence_by_turn
            .entry(turn_id)
            .and_modify(|current| *current = (*current).max(next_sequence_no))
            .or_insert(next_sequence_no);
    }
    for (turn_id, next_sequence_no) in next_sequence_by_turn {
        transaction.execute(
            "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id, turn_id) DO UPDATE SET
                 next_sequence_no = MAX(
                     protocol_turn_sequence_allocators.next_sequence_no,
                     excluded.next_sequence_no
                 )",
            params![
                target_session_id.to_string(),
                turn_id.to_string(),
                next_sequence_no
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn canonical_protocol_snapshot_from_connection(
    connection: &Connection,
    session_id: SessionId,
    history_request: ProtocolPageRequest,
    turn_request: ProtocolPageRequest,
) -> Result<CanonicalProtocolSnapshot, StorageError> {
    let fence = canonical_protocol_fence_from_connection(connection, session_id)?;
    let history = history_item_page_from_connection(
        connection,
        session_id,
        history_request,
        fence.history_count,
    )?;
    let turns =
        turn_item_page_from_connection(connection, session_id, turn_request, fence.turn_count)?;
    let latest_turn_position = latest_turn_position_for_session(connection, session_id)?;
    Ok(CanonicalProtocolSnapshot {
        fence,
        history,
        turns,
        latest_turn_position,
    })
}

fn protocol_source_count(
    connection: &Connection,
    session_id: SessionId,
    table: &'static str,
    source_kind: &'static str,
) -> Result<usize, StorageError> {
    let table_count = connection.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
        params![session_id.to_string()],
        |row| row.get::<_, i64>(0),
    )?;
    let append_count = connection.query_row(
        "SELECT COUNT(*)
         FROM protocol_item_append_order
         WHERE session_id = ?1 AND source_kind = ?2",
        params![session_id.to_string(), source_kind],
        |row| row.get::<_, i64>(0),
    )?;
    let joined_count = connection.query_row(
        &format!(
            "SELECT COUNT(*)
             FROM {table} AS source
             INNER JOIN protocol_item_append_order AS append_order
               ON append_order.session_id = source.session_id
              AND append_order.source_kind = ?2
              AND append_order.source_id = source.id
             WHERE source.session_id = ?1"
        ),
        params![session_id.to_string(), source_kind],
        |row| row.get::<_, i64>(0),
    )?;
    if table_count != append_count || table_count != joined_count {
        return Err(StorageError::Message(format!(
            "canonical protocol append-order invariant failed for session {session_id} source {source_kind}: table has {table_count} rows, append order has {append_count}, joined ownership has {joined_count}"
        )));
    }
    usize::try_from(table_count).map_err(|_| {
        StorageError::Message(format!(
            "canonical protocol {source_kind} count exceeds this platform's page range"
        ))
    })
}

fn history_item_page_from_connection(
    connection: &Connection,
    session_id: SessionId,
    request: ProtocolPageRequest,
    total: usize,
) -> Result<ProtocolPage<HistoryItem>, StorageError> {
    let (requested_offset, limit, after_append_position) = request.resolve(total)?;
    let offset = match after_append_position {
        Some(cursor) => protocol_cursor_offset(connection, session_id, "history_item", cursor)?,
        None => requested_offset,
    };
    let sql_offset = if after_append_position.is_some() {
        0
    } else {
        requested_offset
    };
    let mut statement = connection.prepare(
        "SELECT history.id, history.scope_kind, history.turn_id, history.sequence_no, history.payload_json,
                history.created_at_ms, append_order.append_position
         FROM protocol_history_items AS history
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = history.session_id
          AND append_order.source_kind = 'history_item'
          AND append_order.source_id = history.id
         WHERE history.session_id = ?1
           AND (?4 IS NULL OR append_order.append_position > ?4)
         ORDER BY append_order.append_position ASC
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement.query_map(
        params![
            session_id.to_string(),
            sqlite_page_value(limit),
            sqlite_page_value(sql_offset),
            after_append_position,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;
    let mut items = Vec::new();
    let mut next_cursor = None;
    for row in rows {
        let (id, scope_kind, turn_id, sequence_no, payload_json, created_at_ms, append_position) =
            row?;
        items.push(decode_history_item(
            session_id,
            id,
            scope_kind,
            turn_id,
            sequence_no,
            payload_json,
            created_at_ms,
            "history item page",
        )?);
        next_cursor = Some(append_position);
    }
    Ok(ProtocolPage {
        offset,
        limit,
        total,
        items,
        next_cursor,
    })
}

fn traverse_active_history_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    expected_snapshot: Option<ActiveHistorySnapshot>,
    limit: usize,
    visitor: &mut dyn FnMut(ActiveHistoryPage) -> Result<(), StorageError>,
) -> Result<ActiveHistoryTraversalStats, StorageError> {
    validate_active_history_page_limit(limit)?;
    with_prepared_active_history(transaction, session_id, |snapshot| {
        if let Some(expected) = expected_snapshot {
            ensure_active_history_snapshot(session_id, expected, snapshot)?;
        }

        let mut cursor = None;
        let mut traversed_items = 0usize;
        let mut pages = 0usize;
        let mut max_page_items = 0usize;
        while traversed_items < snapshot.active_count {
            let page = active_history_keyset_page(transaction, session_id, cursor, limit)?;
            let item_count = page.items.len();
            if item_count == 0 {
                return Err(StorageError::Message(format!(
                    "active history traversal for session {session_id} made no progress after {traversed_items} of {} items",
                    snapshot.active_count
                )));
            }
            let next_cursor = page.next_cursor.ok_or_else(|| {
                StorageError::Message(format!(
                    "active history traversal for session {session_id} returned items without a cursor"
                ))
            })?;
            if let Some(previous) = cursor
                && !active_history_cursor_advances(previous, next_cursor)
            {
                return Err(StorageError::Message(format!(
                    "active history traversal cursor for session {session_id} did not advance"
                )));
            }

            traversed_items = traversed_items.saturating_add(item_count);
            if traversed_items > snapshot.active_count {
                return Err(StorageError::Message(format!(
                    "active history traversal for session {session_id} advanced to {traversed_items} beyond {} items",
                    snapshot.active_count
                )));
            }
            pages = pages.saturating_add(1);
            max_page_items = max_page_items.max(item_count);
            let has_more = page.has_more;
            visitor(ActiveHistoryPage {
                items: page.items,
                has_more,
            })?;
            cursor = Some(next_cursor);

            if !has_more {
                break;
            }
        }
        if traversed_items != snapshot.active_count {
            return Err(StorageError::Message(format!(
                "active history traversal for session {session_id} ended after {traversed_items} of {} items",
                snapshot.active_count
            )));
        }
        Ok(ActiveHistoryTraversalStats {
            snapshot,
            pages,
            max_page_items,
        })
    })
}

fn validate_active_history_page_limit(limit: usize) -> Result<(), StorageError> {
    if limit == 0 {
        return Err(StorageError::Message(
            "active history page limit must be greater than zero".to_string(),
        ));
    }
    if limit > MAX_PROTOCOL_PAGE_LIMIT {
        return Err(StorageError::Message(format!(
            "active history page limit {limit} exceeds the maximum {MAX_PROTOCOL_PAGE_LIMIT}"
        )));
    }
    Ok(())
}

fn with_prepared_active_history<T>(
    connection: &Connection,
    session_id: SessionId,
    operation: impl FnOnce(ActiveHistorySnapshot) -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    connection.execute_batch(
        "DROP TABLE IF EXISTS temp.moyai_active_history_traversal;
         CREATE TEMP TABLE temp.moyai_active_history_traversal (
             history_id TEXT PRIMARY KEY,
             effective_position INTEGER NOT NULL,
             append_position INTEGER NOT NULL,
             UNIQUE (effective_position, append_position)
         ) WITHOUT ROWID;
         CREATE INDEX temp.idx_moyai_active_history_traversal_order
             ON moyai_active_history_traversal (effective_position, append_position);",
    )?;
    let result = prepare_active_history_snapshot(connection, session_id).and_then(operation);
    let cleanup =
        connection.execute_batch("DROP TABLE IF EXISTS temp.moyai_active_history_traversal;");
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(StorageError::from(error)),
    }
}

fn prepare_active_history_snapshot(
    connection: &Connection,
    session_id: SessionId,
) -> Result<ActiveHistorySnapshot, StorageError> {
    // Validate append-order ownership exactly once for the traversal. The temp
    // table is transaction-local derived state; canonical history remains the
    // sole durable owner.
    let canonical_count = protocol_source_count(
        connection,
        session_id,
        "protocol_history_items",
        "history_item",
    )?;
    let append_fence = connection.query_row(
        "SELECT MAX(append_position)
         FROM protocol_item_append_order
         WHERE session_id = ?1",
        params![session_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    let steer_count = history_payload_kind_count(connection, session_id, "steer_turn")?;
    let agent_communication_count =
        history_payload_kind_count(connection, session_id, "inter_agent_communication")?;
    let active_count = connection.execute(
        "WITH RECURSIVE replacement_tree(compaction_id, replaced_id) AS (
             SELECT compaction.id, CAST(replacement.value AS TEXT)
             FROM protocol_history_items AS compaction
             JOIN json_each(
                 compaction.payload_json,
                 '$.replacement_item_ids'
             ) AS replacement ON TRUE
             WHERE compaction.session_id = ?1
               AND json_extract(compaction.payload_json, '$.kind') = 'compaction'
             UNION
             SELECT replacement_tree.compaction_id, CAST(replacement.value AS TEXT)
             FROM replacement_tree
             INNER JOIN protocol_history_items AS nested_compaction
               ON nested_compaction.session_id = ?1
              AND nested_compaction.id = replacement_tree.replaced_id
              AND json_extract(nested_compaction.payload_json, '$.kind') = 'compaction'
             JOIN json_each(
                 nested_compaction.payload_json,
                 '$.replacement_item_ids'
             ) AS replacement ON TRUE
         ),
         active_history AS (
             SELECT history.id,
                    append_order.append_position,
                    CASE
                        WHEN json_extract(history.payload_json, '$.kind') = 'compaction'
                        THEN COALESCE(
                            (
                                SELECT MIN(replaced_order.append_position)
                                FROM replacement_tree AS replacement
                                INNER JOIN protocol_item_append_order AS replaced_order
                                  ON replaced_order.session_id = history.session_id
                                 AND replaced_order.source_kind = 'history_item'
                                 AND replaced_order.source_id = replacement.replaced_id
                                WHERE replacement.compaction_id = history.id
                            ),
                            append_order.append_position
                        )
                        ELSE append_order.append_position
                    END AS effective_position
             FROM protocol_history_items AS history
             INNER JOIN protocol_item_append_order AS append_order
               ON append_order.session_id = history.session_id
              AND append_order.source_kind = 'history_item'
              AND append_order.source_id = history.id
             WHERE history.session_id = ?1
               AND NOT EXISTS (
                   SELECT 1
                   FROM protocol_history_items AS compaction
                   JOIN json_each(
                       compaction.payload_json,
                       '$.replacement_item_ids'
                   ) AS replacement ON TRUE
                   WHERE compaction.session_id = history.session_id
                     AND json_extract(compaction.payload_json, '$.kind') = 'compaction'
                     AND CAST(replacement.value AS TEXT) = history.id
               )
         )
         INSERT INTO temp.moyai_active_history_traversal
             (history_id, effective_position, append_position)
         SELECT id, effective_position, append_position
         FROM active_history",
        params![session_id.to_string()],
    )?;
    if active_count > canonical_count {
        return Err(StorageError::Message(format!(
            "active history for session {session_id} contains {active_count} rows but canonical history contains {canonical_count}"
        )));
    }
    Ok(ActiveHistorySnapshot {
        append_fence,
        canonical_count,
        steer_count,
        agent_communication_count,
        active_count,
    })
}

fn active_history_keyset_page(
    connection: &Connection,
    session_id: SessionId,
    cursor: Option<ActiveHistoryCursor>,
    limit: usize,
) -> Result<ActiveHistoryTraversalPage, StorageError> {
    let fetch_limit = limit.saturating_add(1);
    let sql = if cursor.is_some() {
        "SELECT history.id, history.scope_kind, history.turn_id, history.sequence_no,
                history.payload_json, history.created_at_ms,
                traversal.effective_position, traversal.append_position
         FROM temp.moyai_active_history_traversal AS traversal
              INDEXED BY idx_moyai_active_history_traversal_order
         INNER JOIN protocol_history_items AS history
           ON history.session_id = ?1 AND history.id = traversal.history_id
         WHERE (traversal.effective_position, traversal.append_position) > (?2, ?3)
         ORDER BY traversal.effective_position ASC, traversal.append_position ASC
         LIMIT ?4"
    } else {
        "SELECT history.id, history.scope_kind, history.turn_id, history.sequence_no,
                history.payload_json, history.created_at_ms,
                traversal.effective_position, traversal.append_position
         FROM temp.moyai_active_history_traversal AS traversal
              INDEXED BY idx_moyai_active_history_traversal_order
         INNER JOIN protocol_history_items AS history
           ON history.session_id = ?1 AND history.id = traversal.history_id
         ORDER BY traversal.effective_position ASC, traversal.append_position ASC
         LIMIT ?2"
    };
    let mut statement = connection.prepare(sql)?;
    let mut rows = match cursor {
        Some(cursor) => statement.query(params![
            session_id.to_string(),
            cursor.effective_position,
            cursor.append_position,
            sqlite_page_value(fetch_limit),
        ])?,
        None => statement.query(params![
            session_id.to_string(),
            sqlite_page_value(fetch_limit),
        ])?,
    };
    let mut decoded = Vec::with_capacity(fetch_limit);
    while let Some(row) = rows.next()? {
        let (
            id,
            scope_kind,
            turn_id,
            sequence_no,
            payload_json,
            created_at_ms,
            effective_position,
            append_position,
        ) = (
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
        );
        decoded.push((
            decode_history_item(
                session_id,
                id,
                scope_kind,
                turn_id,
                sequence_no,
                payload_json,
                created_at_ms,
                "active history item",
            )?,
            ActiveHistoryCursor {
                effective_position,
                append_position,
            },
        ));
    }
    let has_more = decoded.len() > limit;
    if has_more {
        decoded.truncate(limit);
    }
    let next_cursor = decoded.last().map(|(_, cursor)| *cursor);
    Ok(ActiveHistoryTraversalPage {
        items: decoded.into_iter().map(|(item, _)| item).collect(),
        next_cursor,
        has_more,
    })
}

fn active_history_cursor_advances(
    previous: ActiveHistoryCursor,
    next: ActiveHistoryCursor,
) -> bool {
    next.effective_position > previous.effective_position
        || (next.effective_position == previous.effective_position
            && next.append_position > previous.append_position)
}

fn history_payload_kind_count(
    connection: &Connection,
    session_id: SessionId,
    kind: &'static str,
) -> Result<usize, StorageError> {
    let count = connection.query_row(
        "SELECT COUNT(*)
         FROM protocol_history_items
         WHERE session_id = ?1
           AND json_extract(payload_json, '$.kind') = ?2",
        params![session_id.to_string(), kind],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).map_err(|_| {
        StorageError::Message(format!(
            "canonical history kind `{kind}` count exceeds this platform's range"
        ))
    })
}

fn history_items_by_id_from_connection(
    connection: &Connection,
    session_id: SessionId,
    item_ids: &[HistoryItemId],
) -> Result<Vec<HistoryItem>, StorageError> {
    let unique_ids = item_ids.iter().copied().collect::<HashSet<_>>();
    let encoded_ids = serde_json::to_string(
        &unique_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    )?;
    let mut statement = connection.prepare(
        "SELECT history.id, history.scope_kind, history.turn_id, history.sequence_no, history.payload_json,
                history.created_at_ms
         FROM protocol_history_items AS history
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = history.session_id
          AND append_order.source_kind = 'history_item'
          AND append_order.source_id = history.id
         WHERE history.session_id = ?1
           AND history.id IN (
               SELECT CAST(value AS TEXT) FROM json_each(?2)
           )
         ORDER BY append_order.append_position ASC",
    )?;
    let rows = statement.query_map(params![session_id.to_string(), encoded_ids], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut items = Vec::with_capacity(unique_ids.len());
    for row in rows {
        let (id, scope_kind, turn_id, sequence_no, payload_json, created_at_ms) = row?;
        items.push(decode_history_item(
            session_id,
            id,
            scope_kind,
            turn_id,
            sequence_no,
            payload_json,
            created_at_ms,
            "history item identity query",
        )?);
    }
    if items.len() != unique_ids.len() {
        return Err(StorageError::Message(format!(
            "canonical history identity query for session {session_id} resolved {} of {} items",
            items.len(),
            unique_ids.len()
        )));
    }
    Ok(items)
}

fn parse_session_id(value: &str, context: &str) -> Result<SessionId, StorageError> {
    value.parse::<SessionId>().map_err(|error| {
        StorageError::Message(format!(
            "invalid session id `{value}` in {context}: {error}"
        ))
    })
}

fn optional_input_content(
    payload_json: Option<String>,
) -> Result<Option<Vec<ContentPart>>, StorageError> {
    let Some(payload_json) = payload_json else {
        return Ok(None);
    };
    match serde_json::from_str::<HistoryItemPayload>(&payload_json)? {
        HistoryItemPayload::UserTurn { content, .. }
        | HistoryItemPayload::SteerTurn { content, .. } => Ok(Some(content)),
        payload => Err(StorageError::Message(format!(
            "retained child input projection decoded unexpected history kind `{}`",
            history_payload_kind(&payload)
        ))),
    }
}

fn optional_assistant_content(
    payload_json: Option<String>,
) -> Result<Option<Vec<ContentPart>>, StorageError> {
    let Some(payload_json) = payload_json else {
        return Ok(None);
    };
    match serde_json::from_str::<HistoryItemPayload>(&payload_json)? {
        HistoryItemPayload::AssistantMessage { content, .. } => Ok(Some(content)),
        payload => Err(StorageError::Message(format!(
            "assistant response projection decoded unexpected history kind `{}`",
            history_payload_kind(&payload)
        ))),
    }
}

fn optional_error_message(payload_json: Option<String>) -> Result<Option<String>, StorageError> {
    let Some(payload_json) = payload_json else {
        return Ok(None);
    };
    match serde_json::from_str::<HistoryItemPayload>(&payload_json)? {
        HistoryItemPayload::Error { message } => Ok(Some(message)),
        payload => Err(StorageError::Message(format!(
            "child error projection decoded unexpected history kind `{}`",
            history_payload_kind(&payload)
        ))),
    }
}

fn optional_terminal_interruption_cause(
    payload_json: Option<String>,
) -> Result<Option<TurnInterruptionCause>, StorageError> {
    let Some(payload_json) = payload_json else {
        return Ok(None);
    };
    match serde_json::from_str::<RuntimeEventMsg>(&payload_json)? {
        RuntimeEventMsg::TurnTerminal { terminal } => Ok(terminal.interruption_cause()),
        _ => Err(StorageError::Message(
            "child terminal projection decoded a non-terminal runtime event".to_string(),
        )),
    }
}

fn history_payload_kind(payload: &HistoryItemPayload) -> &'static str {
    match payload {
        HistoryItemPayload::UserTurn { .. } => "user_turn",
        HistoryItemPayload::SteerTurn { .. } => "steer_turn",
        HistoryItemPayload::InterAgentCommunication { .. } => "inter_agent_communication",
        HistoryItemPayload::SubAgentActivity { .. } => "sub_agent_activity",
        HistoryItemPayload::CollaborationModeInstruction { .. } => "collaboration_mode_instruction",
        HistoryItemPayload::AssistantMessage { .. } => "assistant_message",
        HistoryItemPayload::Error { .. } => "error",
        HistoryItemPayload::ToolCall { .. } => "tool_call",
        HistoryItemPayload::ToolOutput { .. } => "tool_output",
        HistoryItemPayload::Compaction { .. } => "compaction",
        _ => "other",
    }
}

fn turn_item_page_from_connection(
    connection: &Connection,
    session_id: SessionId,
    request: ProtocolPageRequest,
    total: usize,
) -> Result<ProtocolPage<TurnItem>, StorageError> {
    let (requested_offset, limit, after_append_position) = request.resolve(total)?;
    let offset = match after_append_position {
        Some(cursor) => protocol_cursor_offset(connection, session_id, "turn_item", cursor)?,
        None => requested_offset,
    };
    let sql_offset = if after_append_position.is_some() {
        0
    } else {
        requested_offset
    };
    let mut statement = connection.prepare(
        "SELECT turn_item.id, turn_item.turn_id, turn_item.source_item_id,
                turn_item.sequence_no, turn_item.payload_json, append_order.append_position
         FROM protocol_turn_items AS turn_item
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = turn_item.session_id
          AND append_order.source_kind = 'turn_item'
          AND append_order.source_id = turn_item.id
         WHERE turn_item.session_id = ?1
           AND (?4 IS NULL OR append_order.append_position > ?4)
         ORDER BY append_order.append_position ASC
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement.query_map(
        params![
            session_id.to_string(),
            sqlite_page_value(limit),
            sqlite_page_value(sql_offset),
            after_append_position,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        },
    )?;
    let mut items = Vec::new();
    let mut next_cursor = None;
    for row in rows {
        let (id, turn_id, source_item_id, sequence_no, payload_json, append_position) = row?;
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
        next_cursor = Some(append_position);
    }
    Ok(ProtocolPage {
        offset,
        limit,
        total,
        items,
        next_cursor,
    })
}

fn runtime_event_page_from_connection(
    connection: &Connection,
    session_id: SessionId,
    request: ProtocolPageRequest,
    total: usize,
) -> Result<ProtocolPage<RuntimeEvent>, StorageError> {
    let (requested_offset, limit, after_append_position) = request.resolve(total)?;
    let offset = match after_append_position {
        Some(cursor) => protocol_cursor_offset(connection, session_id, "runtime_event", cursor)?,
        None => requested_offset,
    };
    let sql_offset = if after_append_position.is_some() {
        0
    } else {
        requested_offset
    };
    let mut statement = connection.prepare(
        "SELECT runtime_event.id, runtime_event.turn_id, runtime_event.sequence_no,
                runtime_event.msg_json, runtime_event.created_at_ms,
                append_order.append_position
         FROM protocol_runtime_events AS runtime_event
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = runtime_event.session_id
          AND append_order.source_kind = 'runtime_event'
          AND append_order.source_id = runtime_event.id
         WHERE runtime_event.session_id = ?1
           AND (?4 IS NULL OR append_order.append_position > ?4)
         ORDER BY append_order.append_position ASC
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement.query_map(
        params![
            session_id.to_string(),
            sqlite_page_value(limit),
            sqlite_page_value(sql_offset),
            after_append_position,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        },
    )?;
    let mut items = Vec::new();
    let mut next_cursor = None;
    for row in rows {
        let (id, turn_id, sequence_no, msg_json, created_at_ms, append_position) = row?;
        items.push(RuntimeEvent {
            id: parse_protocol_id::<RuntimeEventId>(&id, "runtime event")?,
            session_id,
            turn_id: parse_protocol_id::<TurnId>(&turn_id, "runtime event turn")?,
            sequence_no,
            created_at_ms,
            msg: serde_json::from_str::<RuntimeEventMsg>(&msg_json)?,
        });
        next_cursor = Some(append_position);
    }
    Ok(ProtocolPage {
        offset,
        limit,
        total,
        items,
        next_cursor,
    })
}

fn protocol_cursor_offset(
    connection: &Connection,
    session_id: SessionId,
    source_kind: &'static str,
    append_position: i64,
) -> Result<usize, StorageError> {
    let count = connection.query_row(
        "SELECT COUNT(*)
         FROM protocol_item_append_order
         WHERE session_id = ?1
           AND source_kind = ?2
           AND append_position <= ?3",
        params![session_id.to_string(), source_kind, append_position],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).map_err(|_| {
        StorageError::Message(format!(
            "protocol cursor offset for source `{source_kind}` exceeds this platform's range"
        ))
    })
}

fn latest_page_offset(total: usize, limit: usize) -> usize {
    total.saturating_sub(limit)
}

fn sqlite_page_value(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
fn list_history_items_for_session_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Vec<HistoryItem>, StorageError> {
    let total = protocol_source_count(
        connection,
        session_id,
        "protocol_history_items",
        "history_item",
    )?;
    let mut statement = connection.prepare(
        "SELECT history.id, history.scope_kind, history.turn_id, history.sequence_no, history.payload_json, history.created_at_ms
         FROM protocol_history_items AS history
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = history.session_id
          AND append_order.source_kind = 'history_item'
          AND append_order.source_id = history.id
         WHERE history.session_id = ?1
         ORDER BY append_order.append_position ASC",
    )?;
    let rows = statement.query_map(params![session_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut items = Vec::with_capacity(total);
    for row in rows {
        let (id, scope_kind, turn_id, sequence_no, payload_json, created_at_ms) = row?;
        items.push(decode_history_item(
            session_id,
            id,
            scope_kind,
            turn_id,
            sequence_no,
            payload_json,
            created_at_ms,
            "history item session list",
        )?);
    }
    Ok(items)
}

#[cfg(test)]
fn list_turn_items_for_session_from_connection(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Vec<TurnItem>, StorageError> {
    let total = protocol_source_count(connection, session_id, "protocol_turn_items", "turn_item")?;
    let mut statement = connection.prepare(
        "SELECT turn_item.id, turn_item.turn_id, turn_item.source_item_id,
                turn_item.sequence_no, turn_item.payload_json
         FROM protocol_turn_items AS turn_item
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = turn_item.session_id
          AND append_order.source_kind = 'turn_item'
          AND append_order.source_id = turn_item.id
         WHERE turn_item.session_id = ?1
         ORDER BY append_order.append_position ASC",
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
    let mut items = Vec::with_capacity(total);
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

fn fork_history_payload_for_session(
    transaction: &Transaction<'_>,
    source_session_id: SessionId,
    target_session_id: SessionId,
    payload: HistoryItemPayload,
) -> Result<HistoryItemPayload, StorageError> {
    match payload {
        HistoryItemPayload::Compaction {
            mode,
            summary,
            replacement_item_ids,
        } => {
            let replacement_item_ids = replacement_item_ids
                .into_iter()
                .map(|source_id| {
                    canonical_fork_target_history_id(
                        transaction,
                        source_session_id,
                        target_session_id,
                        source_id,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(HistoryItemPayload::Compaction {
                mode,
                summary,
                replacement_item_ids,
            })
        }
        other => Ok(other),
    }
}

pub(crate) fn latest_protocol_turn_ids_in_transaction(
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
             AND scope_kind = 'turn'
             AND turn_id IS NOT NULL
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

pub(crate) fn latest_turn_position_for_session(
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
           SELECT sequence_no FROM protocol_history_items
            WHERE session_id = ?1 AND scope_kind = 'turn' AND turn_id = ?2
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
           AND scope_kind = 'turn'
           AND turn_id IS NOT NULL
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
        HistoryScope::Turn {
            turn_id: event.turn_id,
        },
        event.sequence_no,
        "runtime_event",
        &event.id.to_string(),
        event.created_at_ms,
    )?;
    Ok(())
}

pub(crate) fn insert_session_owned_event_bundle_in_transaction(
    _authority: &SessionProtocolWriteAuthority,
    transaction: &Transaction<'_>,
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<StoredProtocolEventBundle, StorageError> {
    insert_event_bundle_unchecked(transaction, event, history_item, turn_item)
}

pub(crate) fn insert_idle_inter_agent_history_in_transaction(
    _authority: &SessionProtocolWriteAuthority,
    transaction: &Transaction<'_>,
    session_id: SessionId,
    communication: crate::protocol::InterAgentCommunication,
) -> Result<HistoryItemId, StorageError> {
    let item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        scope: HistoryScope::Session,
        sequence_no: claim_session_history_sequence_in_transaction(transaction, session_id, 0)?,
        created_at_ms: SystemClock::now_ms(),
        payload: HistoryItemPayload::InterAgentCommunication { communication },
    };
    insert_history_item(transaction, &item)?;
    Ok(item.id)
}

fn insert_event_bundle_unchecked(
    transaction: &Transaction<'_>,
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<StoredProtocolEventBundle, StorageError> {
    validate_event_bundle_coherence(event, history_item, turn_item)?;
    let sequence_no = claim_protocol_sequence_in_transaction(
        transaction,
        event.session_id,
        event.turn_id,
        event.sequence_no,
    )?;
    let mut runtime_event = event.clone();
    runtime_event.sequence_no = sequence_no;
    let history_item = history_item.map(|item| {
        let mut item = item.clone();
        item.sequence_no = sequence_no;
        item
    });
    let turn_item = turn_item.map(|item| {
        let mut item = item.clone();
        item.sequence_no = sequence_no;
        item
    });
    insert_runtime_event(transaction, &runtime_event)?;
    if let Some(history_item) = &history_item {
        insert_history_item(transaction, history_item)?;
    }
    if let Some(turn_item) = &turn_item {
        insert_turn_item(transaction, turn_item)?;
    }
    Ok(StoredProtocolEventBundle {
        runtime_event,
        history_item,
    })
}

fn validate_recording_projection(
    event: &RuntimeEvent,
    history_item: Option<&HistoryItem>,
    turn_item: Option<&TurnItem>,
) -> Result<(), StorageError> {
    validate_event_bundle_coherence(event, history_item, turn_item)?;
    let history_payload = history_item.map(|item| &item.payload);
    let turn_payload = turn_item.map(|item| &item.payload);
    let allowed = match (&event.msg, history_payload, turn_payload) {
        // Session lifecycle notices are runtime-only. They do not own canonical
        // user/model/tool state.
        (RuntimeEventMsg::Warning { .. }, None, None) => true,
        (
            RuntimeEventMsg::Warning { message },
            Some(HistoryItemPayload::Error {
                message: history_message,
            }),
            Some(TurnItemPayload::Error {
                message: turn_message,
            }),
        ) => message == history_message && message == turn_message,
        (
            RuntimeEventMsg::ModelRequestPrepared { .. },
            Some(HistoryItemPayload::RequestDiagnostics { .. }),
            None,
        ) => true,
        (
            RuntimeEventMsg::WorldStateUpdated { .. },
            Some(HistoryItemPayload::WorldState { .. }),
            Some(TurnItemPayload::WorldState { .. }),
        ) => true,
        (
            RuntimeEventMsg::ApprovalRequested { call_id, .. },
            None,
            Some(TurnItemPayload::ApprovalRequest {
                call_id: turn_call_id,
                ..
            }),
        ) => call_id == turn_call_id,
        (
            RuntimeEventMsg::ApprovalResolved { call_id, .. },
            Some(HistoryItemPayload::ApprovalDecision {
                call_id: history_call_id,
                ..
            }),
            None,
        ) => call_id == history_call_id,
        (
            RuntimeEventMsg::ContextCompacted { item_id, mode },
            Some(HistoryItemPayload::Compaction {
                mode: history_mode, ..
            }),
            Some(TurnItemPayload::ContextCompaction { .. }),
        ) => history_item.is_some_and(|item| item.id == *item_id) && mode == history_mode,
        _ => false,
    };
    if allowed {
        return Ok(());
    }
    Err(StorageError::Message(format!(
        "protocol recording sink cannot own runtime projection `{}`; use its atomic state owner",
        runtime_event_kind(&event.msg)
    )))
}

fn runtime_event_kind(message: &RuntimeEventMsg) -> &'static str {
    match message {
        RuntimeEventMsg::UserInputAccepted { .. } => "user_input_accepted",
        RuntimeEventMsg::SteerInputAccepted { .. } => "steer_input_accepted",
        RuntimeEventMsg::InterAgentCommunicationReceived { .. } => {
            "inter_agent_communication_received"
        }
        RuntimeEventMsg::SubAgentActivity { .. } => "sub_agent_activity",
        RuntimeEventMsg::AssistantMessageCommitted { .. } => "assistant_message_committed",
        RuntimeEventMsg::ModelRequestPrepared { .. } => "model_request_prepared",
        RuntimeEventMsg::WorldStateUpdated { .. } => "world_state_updated",
        RuntimeEventMsg::ToolLifecycle { .. } => "tool_lifecycle",
        RuntimeEventMsg::ApprovalRequested { .. } => "approval_requested",
        RuntimeEventMsg::ApprovalResolved { .. } => "approval_resolved",
        RuntimeEventMsg::ContextCompacted { .. } => "context_compacted",
        RuntimeEventMsg::FileChangesRecorded { .. } => "file_changes_recorded",
        RuntimeEventMsg::Warning { .. } => "warning",
        RuntimeEventMsg::TurnTerminal { .. } => "turn_terminal",
    }
}

fn claim_protocol_sequence_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    turn_id: TurnId,
    requested_sequence_no: i64,
) -> Result<i64, StorageError> {
    let stored_max = transaction.query_row(
        "SELECT MAX(sequence_no)
         FROM (
             SELECT sequence_no FROM protocol_runtime_events WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL
             SELECT sequence_no FROM protocol_history_items WHERE session_id = ?1 AND turn_id = ?2
             UNION ALL
             SELECT sequence_no FROM protocol_turn_items WHERE session_id = ?1 AND turn_id = ?2
         )",
        params![session_id.to_string(), turn_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    let sequence_floor = requested_sequence_no
        .max(0)
        .max(stored_max.unwrap_or(-1).saturating_add(1));
    let claimed = transaction.query_row(
        "INSERT INTO protocol_turn_sequence_allocators
             (session_id, turn_id, next_sequence_no)
         VALUES (?1, ?2, ?3 + 1)
         ON CONFLICT(session_id, turn_id) DO UPDATE SET
             next_sequence_no = MAX(
                 protocol_turn_sequence_allocators.next_sequence_no,
                 excluded.next_sequence_no - 1
             ) + 1
         RETURNING next_sequence_no - 1",
        params![session_id.to_string(), turn_id.to_string(), sequence_floor],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(claimed)
}

fn claim_session_history_sequence_in_transaction(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    requested_sequence_no: i64,
) -> Result<i64, StorageError> {
    let stored_max = transaction.query_row(
        "SELECT MAX(sequence_no)
         FROM protocol_history_items
         WHERE session_id = ?1 AND scope_kind = 'session'",
        params![session_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(requested_sequence_no
        .max(0)
        .max(stored_max.unwrap_or(-1).saturating_add(1)))
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
    if history_item.scope
        != (HistoryScope::Turn {
            turn_id: event.turn_id,
        })
    {
        return Err(StorageError::Message(format!(
            "protocol event bundle turn mismatch: event turn `{}` history item scope `{:?}`",
            event.turn_id, history_item.scope
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
    if history_item.scope
        != (HistoryScope::Turn {
            turn_id: turn_item.turn_id,
        })
    {
        return Err(StorageError::Message(format!(
            "protocol history-turn bundle turn mismatch: history item scope `{:?}` turn item turn `{}`",
            history_item.scope, turn_item.turn_id
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
    let scope_kind = item.scope.as_str();
    let turn_id = item.turn_id().map(|turn_id| turn_id.to_string());
    connection.execute_protocol(
        "INSERT INTO protocol_history_items
            (id, session_id, scope_kind, turn_id, sequence_no, payload_json, payload_sha256, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        &[
            &item.id.to_string(),
            &item.session_id.to_string(),
            &scope_kind,
            &turn_id,
            &item.sequence_no,
            &payload_json,
            &hash_text(&payload_json),
            &item.created_at_ms,
        ],
    )?;
    insert_protocol_append_order(
        connection,
        item.session_id,
        item.scope,
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
        HistoryScope::Turn {
            turn_id: item.turn_id,
        },
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
    scope: HistoryScope,
    sequence_no: i64,
    source_kind: &str,
    source_id: &str,
    created_at_ms: i64,
) -> Result<(), StorageError> {
    let scope_kind = scope.as_str();
    let turn_id = scope.turn_id().map(|turn_id| turn_id.to_string());
    connection.execute_protocol(
        "INSERT OR IGNORE INTO protocol_item_append_order
            (session_id, scope_kind, turn_id, sequence_no, source_kind, source_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            &session_id.to_string(),
            &scope_kind,
            &turn_id,
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

fn decode_history_item(
    session_id: SessionId,
    id: String,
    scope_kind: String,
    turn_id: Option<String>,
    sequence_no: i64,
    payload_json: String,
    created_at_ms: i64,
    label: &str,
) -> Result<HistoryItem, StorageError> {
    let scope = match (scope_kind.as_str(), turn_id) {
        ("turn", Some(turn_id)) => HistoryScope::Turn {
            turn_id: parse_protocol_id::<TurnId>(&turn_id, &format!("{label} turn"))?,
        },
        ("session", None) => HistoryScope::Session,
        (scope_kind, turn_id) => {
            return Err(StorageError::Message(format!(
                "invalid protocol {label} scope `{scope_kind}` with turn id {turn_id:?}"
            )));
        }
    };
    Ok(HistoryItem {
        id: parse_protocol_id::<HistoryItemId>(&id, label)?,
        session_id,
        scope,
        sequence_no,
        created_at_ms,
        payload: serde_json::from_str::<HistoryItemPayload>(&payload_json)?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::Duration;

    use rusqlite::Connection;

    use super::*;
    use crate::protocol::ContentPart;

    fn seed_fork_sessions(connection: &Connection, session_ids: &[SessionId]) {
        let project_id = crate::session::ProjectId::new();
        connection
            .execute(
                "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES (?1, 'C:/fork-fixture', 'fork fixture', 'none', 1, 1)",
                params![project_id.to_string()],
            )
            .expect("fork project fixture");
        for (index, session_id) in session_ids.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES (?1, ?2, ?3, 'idle', 'C:/fork-fixture', 'model',
                             'http://localhost', ?4, ?4, NULL)",
                    params![
                        session_id.to_string(),
                        project_id.to_string(),
                        format!("fork session {index}"),
                        i64::try_from(index).unwrap_or(i64::MAX).saturating_add(2)
                    ],
                )
                .expect("fork session fixture");
        }
    }

    fn seed_history_batch_for_test(store: &SqliteProtocolEventStore, items: &[HistoryItem]) {
        let mut connection = store.connection.lock().expect("sqlite mutex");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("history batch transaction");
        for item in items {
            let sequence_no = match item.turn_id() {
                Some(turn_id) => claim_protocol_sequence_in_transaction(
                    &transaction,
                    item.session_id,
                    turn_id,
                    item.sequence_no,
                )
                .expect("history batch sequence"),
                None => item.sequence_no,
            };
            let mut stored_item = item.clone();
            stored_item.sequence_no = sequence_no;
            insert_history_item(&transaction, &stored_item).expect("history batch insert");
        }
        transaction.commit().expect("history batch commit");
    }

    fn seed_turn_batch_for_test(store: &SqliteProtocolEventStore, items: &[TurnItem]) {
        let mut connection = store.connection.lock().expect("sqlite mutex");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("turn batch transaction");
        for item in items {
            let sequence_no = claim_protocol_sequence_in_transaction(
                &transaction,
                item.session_id,
                item.turn_id,
                item.sequence_no,
            )
            .expect("turn batch sequence");
            let mut stored_item = item.clone();
            stored_item.sequence_no = sequence_no;
            insert_turn_item(&transaction, &stored_item).expect("turn batch insert");
        }
        transaction.commit().expect("turn batch commit");
    }

    fn active_history_pages_for_test(
        store: &SqliteProtocolEventStore,
        session_id: SessionId,
        limit: usize,
    ) -> Result<(ActiveHistorySnapshot, Vec<ActiveHistoryPage>), StorageError> {
        let mut pages = Vec::new();
        let snapshot =
            store.visit_active_history_pages_for_session(session_id, limit, &mut |page| {
                pages.push(page);
                Ok(())
            })?;
        Ok((snapshot, pages))
    }

    #[test]
    fn latest_page_is_an_exact_tail_and_all_page_requests_share_one_limit() {
        for (total, expected_offset) in [
            (99, 0),
            (100, 0),
            (101, 1),
            (199, 99),
            (200, 100),
            (201, 101),
        ] {
            let (offset, limit, cursor) = ProtocolPageRequest::Latest { limit: 100 }
                .resolve(total)
                .expect("valid latest page");
            assert_eq!(offset, expected_offset, "total={total}");
            assert_eq!(limit, 100);
            assert_eq!(cursor, None);
        }

        ProtocolPageRequest::After {
            append_position: None,
            limit: MAX_PROTOCOL_PAGE_LIMIT,
        }
        .resolve(201)
        .expect("the shared maximum is accepted");
        let error = ProtocolPageRequest::Offset {
            offset: 0,
            limit: MAX_PROTOCOL_PAGE_LIMIT + 1,
        }
        .resolve(201)
        .expect_err("oversized pages must fail before SQL execution");
        assert!(error.to_string().contains("exceeds the maximum"));
    }

    #[test]
    fn collaboration_mode_is_history_owned_noop_safe_and_resume_replayable() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(Arc::clone(&connection));
        let session_id = SessionId::new();

        assert_eq!(
            store
                .collaboration_mode_for_session(session_id)
                .expect("initial mode"),
            ModeKind::Default
        );
        assert!(
            store
                .set_collaboration_mode(session_id, ModeKind::Default)
                .expect("same default")
                .is_none(),
            "the protocol default is already effective and must not create state"
        );

        let plan_item = store
            .set_collaboration_mode(session_id, ModeKind::Plan)
            .expect("set plan")
            .expect("new plan instruction");
        assert!(matches!(
            plan_item.payload,
            HistoryItemPayload::CollaborationModeInstruction {
                mode: ModeKind::Plan
            }
        ));
        assert_eq!(plan_item.scope, HistoryScope::Session);
        assert_eq!(plan_item.turn_id(), None);
        assert!(
            store
                .set_collaboration_mode(session_id, ModeKind::Plan)
                .expect("same plan")
                .is_none(),
            "same-value updates must not append another instruction"
        );

        let resumed = SqliteProtocolEventStore::new(connection);
        assert_eq!(
            resumed
                .collaboration_mode_for_session(session_id)
                .expect("replayed mode"),
            ModeKind::Plan,
            "a new runtime owner must rehydrate mode solely from durable history"
        );
        assert_eq!(
            resumed
                .list_history_items_for_session(session_id)
                .expect("mode history")
                .iter()
                .filter(|item| matches!(
                    &item.payload,
                    HistoryItemPayload::CollaborationModeInstruction { .. }
                ))
                .count(),
            1
        );

        let default_item = resumed
            .set_collaboration_mode(session_id, ModeKind::Default)
            .expect("restore default")
            .expect("default instruction");
        assert_eq!(default_item.scope, HistoryScope::Session);
        assert_eq!(
            resumed
                .collaboration_mode_for_session(session_id)
                .expect("restored mode"),
            ModeKind::Default
        );
    }

    #[test]
    fn collaboration_mode_lookup_is_one_indexed_row_after_long_history() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(Arc::clone(&connection));
        let session_id = SessionId::new();
        let history_turn = TurnId::new();
        let first_half = (0..(MAX_PROTOCOL_PAGE_LIMIT * 2))
            .map(|index| {
                history_user_turn(
                    session_id,
                    history_turn,
                    i64::try_from(index).unwrap_or(i64::MAX),
                    i64::try_from(index).unwrap_or(i64::MAX),
                    &format!("prefix {index}"),
                )
            })
            .collect::<Vec<_>>();
        seed_history_batch_for_test(&store, &first_half);
        store
            .set_collaboration_mode(session_id, ModeKind::Plan)
            .expect("set indexed mode")
            .expect("mode item");
        let second_half = (0..(MAX_PROTOCOL_PAGE_LIMIT * 2))
            .map(|index| {
                let sequence = index.saturating_add(MAX_PROTOCOL_PAGE_LIMIT * 2);
                history_user_turn(
                    session_id,
                    history_turn,
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    &format!("suffix {index}"),
                )
            })
            .collect::<Vec<_>>();
        seed_history_batch_for_test(&store, &second_half);

        assert_eq!(
            store
                .collaboration_mode_for_session(session_id)
                .expect("latest indexed mode"),
            ModeKind::Plan
        );
        let query_plan = {
            let locked = connection.lock().expect("sqlite mutex");
            let sql = format!("EXPLAIN QUERY PLAN {LATEST_COLLABORATION_MODE_SQL}");
            let mut statement = locked.prepare(&sql).expect("mode query plan");
            let rows = statement
                .query_map(params![session_id.to_string()], |row| {
                    row.get::<_, String>(3)
                })
                .expect("mode plan rows");
            rows.collect::<Result<Vec<_>, _>>()
                .expect("mode plan details")
        };
        assert!(
            query_plan.first().is_some_and(
                |detail| detail.contains("idx_protocol_history_collaboration_mode_session")
            ),
            "latest-mode admission query must start from the partial history index: {query_plan:?}"
        );
        assert_eq!(
            connection
                .lock()
                .expect("sqlite mutex")
                .query_row(
                    "SELECT COUNT(*) FROM protocol_history_items WHERE session_id = ?1",
                    params![session_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("long history count"),
            i64::try_from(MAX_PROTOCOL_PAGE_LIMIT * 4 + 1).unwrap_or(i64::MAX)
        );
    }

    #[test]
    fn active_history_replays_session_scope_without_inventing_a_latest_turn() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let real_turn_id = TurnId::new();
        let user = history_user_turn(session_id, real_turn_id, 0, 10, "real request");
        store
            .seed_history_item_for_test(&user)
            .expect("real turn history");
        let mode = store
            .set_collaboration_mode(session_id, ModeKind::Plan)
            .expect("mode append")
            .expect("mode instruction");
        let mail = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Session,
            sequence_no: 0,
            created_at_ms: 30,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: crate::protocol::InterAgentCommunication {
                    author: "/root/worker".to_string(),
                    recipient: "/root".to_string(),
                    content: "future evidence".to_string(),
                    trigger_turn: false,
                },
            },
        };
        store
            .seed_history_item_for_test(&mail)
            .expect("session mail history");

        let (_snapshot, active_pages) =
            active_history_pages_for_test(&store, session_id, 10).expect("active history");
        let active = active_pages
            .into_iter()
            .flat_map(|page| page.items)
            .collect::<Vec<_>>();
        assert_eq!(
            active.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![user.id, mode.id, mail.id]
        );
        assert_eq!(active[0].turn_id(), Some(real_turn_id));
        assert!(
            active[1..]
                .iter()
                .all(|item| item.scope == HistoryScope::Session)
        );
        assert_eq!(
            store
                .latest_turn_position_for_session(session_id)
                .expect("latest real turn"),
            Some((real_turn_id, 1))
        );
    }

    #[test]
    fn list_history_items_for_session_uses_append_order_across_turns() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let older = history_user_turn(session_id, TurnId::new(), 29, 100, "older-stage");
        let newer = history_user_turn(session_id, TurnId::new(), 15, 200, "newer-stage");

        store
            .seed_history_item_for_test(&older)
            .expect("older insert");
        store
            .seed_history_item_for_test(&newer)
            .expect("newer insert");

        let listed = store
            .list_history_items_for_session(session_id)
            .expect("history list");
        assert_eq!(
            listed.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![older.id, newer.id]
        );
    }

    #[test]
    fn canonical_snapshot_returns_latest_pages_counts_and_one_append_fence() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let first_history = history_user_turn(session_id, first_turn, 0, 100, "first");
        let second_history = history_user_turn(session_id, second_turn, 0, 200, "second");
        let first_turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id: first_turn,
            source_item_id: Some(first_history.id),
            sequence_no: 0,
            payload: TurnItemPayload::UserMessage {
                text: "first".to_string(),
            },
        };
        let second_turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id: second_turn,
            source_item_id: Some(second_history.id),
            sequence_no: 0,
            payload: TurnItemPayload::UserMessage {
                text: "second".to_string(),
            },
        };
        for (history, turn_item, runtime) in [
            (
                &first_history,
                &first_turn_item,
                warning_event(session_id, first_turn, 0, "first"),
            ),
            (
                &second_history,
                &second_turn_item,
                warning_event(session_id, second_turn, 0, "second"),
            ),
        ] {
            store
                .seed_history_item_for_test(history)
                .expect("history append");
            store
                .seed_turn_item_for_test(turn_item)
                .expect("turn append");
            store
                .seed_runtime_event_for_test(&runtime)
                .expect("runtime append");
        }

        let snapshot = store
            .canonical_snapshot_for_session(
                session_id,
                ProtocolPageRequest::Latest { limit: 1 },
                ProtocolPageRequest::Latest { limit: 1 },
            )
            .expect("canonical snapshot");

        assert_eq!(snapshot.fence.history_count, 2);
        assert_eq!(snapshot.fence.turn_count, 2);
        assert_eq!(snapshot.fence.runtime_event_count, 2);
        assert!(snapshot.fence.append_position.is_some());
        assert_eq!(snapshot.history.offset, 1);
        assert_eq!(snapshot.history.items[0].id, second_history.id);
        assert_eq!(snapshot.turns.offset, 1);
        assert_eq!(snapshot.turns.items[0].id, second_turn_item.id);
        assert_eq!(snapshot.latest_turn_position, Some((second_turn, 3)));

        let runtime_page = store
            .runtime_event_page_for_session(session_id, 1, 1)
            .expect("runtime page");
        assert_eq!(runtime_page.total, 2);
        assert_eq!(runtime_page.offset, 1);
        assert_eq!(runtime_page.items[0].turn_id, second_turn);

        let first_cursor_page = store
            .runtime_event_cursor_page_for_session(session_id, None, 1)
            .expect("first cursor page");
        assert_eq!(first_cursor_page.offset, 0);
        assert_eq!(first_cursor_page.items[0].turn_id, first_turn);
        let second_cursor_page = store
            .runtime_event_cursor_page_for_session(session_id, first_cursor_page.next_cursor, 1)
            .expect("second cursor page");
        assert_eq!(second_cursor_page.offset, 1);
        assert_eq!(second_cursor_page.items[0].turn_id, second_turn);
    }

    #[test]
    fn active_history_hydration_pages_hide_replaced_rows_and_preserve_summary_position() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let first = history_user_turn(session_id, turn_id, 0, 100, "first");
        let second = history_user_turn(session_id, turn_id, 1, 200, "second");
        let tail = history_user_turn(session_id, turn_id, 2, 300, "tail");
        for item in [&first, &second, &tail] {
            store
                .seed_history_item_for_test(item)
                .expect("history append");
        }
        let summary = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 3,
            created_at_ms: 400,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "first and second summarized".to_string(),
                replacement_item_ids: vec![first.id, second.id],
            },
        };
        store
            .seed_history_item_for_test(&summary)
            .expect("compaction append");

        let (snapshot, pages) =
            active_history_pages_for_test(&store, session_id, 1).expect("active pages");
        assert_eq!(snapshot.canonical_count, 4);
        assert_eq!(snapshot.active_count, 2);
        assert!(snapshot.append_fence.is_some());
        assert_eq!(pages.len(), 2);
        assert!(pages[0].has_more);
        assert!(!pages[1].has_more);
        assert_eq!(pages[0].items[0].id, summary.id);
        assert_eq!(pages[1].items[0].id, tail.id);

        let nested_summary = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 4,
            created_at_ms: 500,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "nested summary".to_string(),
                replacement_item_ids: vec![summary.id],
            },
        };
        store
            .seed_history_item_for_test(&nested_summary)
            .expect("nested compaction append");
        let (nested_snapshot, nested_pages) =
            active_history_pages_for_test(&store, session_id, 1).expect("nested active pages");
        assert_eq!(nested_snapshot.canonical_count, 5);
        assert_eq!(nested_snapshot.active_count, 2);
        assert_eq!(nested_pages[0].items[0].id, nested_summary.id);
        assert_eq!(nested_pages[1].items[0].id, tail.id);

        let selected = store
            .history_items_by_id(session_id, &[tail.id, nested_summary.id])
            .expect("bounded identity read");
        assert_eq!(
            selected.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![tail.id, nested_summary.id],
            "identity reads follow durable append order, independent of model ordering"
        );
    }

    #[test]
    fn fork_remaps_compaction_replacement_history_ids() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        let replaced = history_user_turn(source_session_id, turn_id, 0, 100, "old detail");
        let compaction = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 200,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "old detail summary".to_string(),
                replacement_item_ids: vec![replaced.id],
            },
        };
        store
            .seed_history_item_for_test(&replaced)
            .expect("replaced item");
        store
            .seed_history_item_for_test(&compaction)
            .expect("compaction item");

        let copied = store
            .fork_canonical_items(source_session_id, target_session_id)
            .expect("fork canonical items");
        let forked = store
            .list_history_items_for_session(target_session_id)
            .expect("forked history");

        assert_eq!(copied, (2, 0));
        assert_ne!(forked[0].id, replaced.id);
        let HistoryItemPayload::Compaction {
            replacement_item_ids,
            ..
        } = &forked[1].payload
        else {
            panic!("second forked item should be compaction");
        };
        assert_eq!(replacement_item_ids.as_slice(), &[forked[0].id]);
        assert!(!replacement_item_ids.contains(&replaced.id));
    }

    #[test]
    fn canonical_fork_streams_more_than_two_history_and_turn_pages_with_exact_identity() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        let item_count = MAX_PROTOCOL_PAGE_LIMIT.saturating_mul(2).saturating_add(17);
        let mut source_history = (0..item_count)
            .map(|index| {
                history_user_turn(
                    source_session_id,
                    turn_id,
                    i64::try_from(index).unwrap_or(i64::MAX),
                    i64::try_from(index).unwrap_or(i64::MAX),
                    &format!("history {index}"),
                )
            })
            .collect::<Vec<_>>();
        let replacement_item_ids = vec![
            source_history[0].id,
            source_history[MAX_PROTOCOL_PAGE_LIMIT + 5].id,
        ];
        source_history.push(HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Session,
            sequence_no: 0,
            created_at_ms: i64::try_from(item_count).unwrap_or(i64::MAX),
            payload: HistoryItemPayload::CollaborationModeInstruction {
                mode: ModeKind::Plan,
            },
        });
        source_history.push(HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: i64::try_from(item_count).unwrap_or(i64::MAX),
            created_at_ms: i64::try_from(item_count.saturating_add(1)).unwrap_or(i64::MAX),
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "cross-page history summary".to_string(),
                replacement_item_ids: replacement_item_ids.clone(),
            },
        });
        let history_count = source_history.len();
        let source_turns = source_history
            .iter()
            .filter_map(|history| history.turn_id().map(|turn_id| (history, turn_id)))
            .enumerate()
            .map(|(index, (history, turn_id))| TurnItem {
                id: TurnItemId::new(),
                session_id: source_session_id,
                turn_id,
                source_item_id: Some(history.id),
                sequence_no: i64::try_from(index).unwrap_or(i64::MAX),
                payload: TurnItemPayload::UserMessage {
                    text: format!("turn {index}"),
                },
            })
            .collect::<Vec<_>>();
        let turn_count = source_turns.len();
        seed_history_batch_for_test(&store, &source_history);
        seed_turn_batch_for_test(&store, &source_turns);

        let stats = store
            .fork_canonical_items_with_stats_for_test(source_session_id, target_session_id)
            .expect("bounded canonical fork");
        assert_eq!(stats.source_fence.history_count, history_count);
        assert_eq!(stats.source_fence.turn_count, turn_count);
        assert_eq!(stats.copied_history_items, history_count);
        assert_eq!(stats.copied_turn_items, turn_count);
        assert_eq!(stats.history_mapping_pages, 3);
        assert_eq!(stats.history_copy_pages, 3);
        assert_eq!(stats.turn_copy_pages, 3);
        assert_eq!(stats.max_source_page_items, MAX_PROTOCOL_PAGE_LIMIT);

        let source_history_after = store
            .list_history_items_for_session(source_session_id)
            .expect("source history after fork");
        let source_turns_after = store
            .list_turn_items_for_session(source_session_id)
            .expect("source turns after fork");
        let target_history = store
            .list_history_items_for_session(target_session_id)
            .expect("target history");
        let target_turns = store
            .list_turn_items_for_session(target_session_id)
            .expect("target turns");
        assert_eq!(source_history_after.len(), history_count);
        assert_eq!(source_turns_after.len(), turn_count);
        assert_eq!(target_history.len(), history_count);
        assert_eq!(target_turns.len(), turn_count);
        assert_eq!(
            source_history_after
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            source_history
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            "forking must not rewrite source history identity"
        );
        assert_eq!(
            source_turns_after
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            source_turns.iter().map(|item| item.id).collect::<Vec<_>>(),
            "forking must not rewrite source turn identity"
        );
        assert_eq!(
            target_history
                .iter()
                .map(|item| item.scope)
                .collect::<Vec<_>>(),
            source_history_after
                .iter()
                .map(|item| item.scope)
                .collect::<Vec<_>>(),
            "forking must preserve the typed history scope"
        );
        assert!(target_history.iter().any(|item| {
            item.scope == HistoryScope::Session
                && matches!(
                    &item.payload,
                    HistoryItemPayload::CollaborationModeInstruction {
                        mode: ModeKind::Plan
                    }
                )
        }));
        let source_history_ids = source_history_after
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        let target_history_ids = target_history
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        assert!(source_history_ids.is_disjoint(&target_history_ids));
        let HistoryItemPayload::Compaction {
            replacement_item_ids: forked_replacement_ids,
            ..
        } = &target_history.last().expect("forked compaction").payload
        else {
            panic!("the final target history item must remain a compaction");
        };
        assert_eq!(forked_replacement_ids.len(), replacement_item_ids.len());
        assert!(
            forked_replacement_ids
                .iter()
                .all(|item_id| target_history_ids.contains(item_id))
        );
        assert!(
            forked_replacement_ids
                .iter()
                .all(|item_id| !replacement_item_ids.contains(item_id)),
            "cross-page compaction lineage must use target history identities"
        );
        assert!(
            target_history
                .iter()
                .all(|item| item.session_id == target_session_id)
        );
        assert!(target_turns.iter().all(|item| {
            item.session_id == target_session_id
                && item
                    .source_item_id
                    .is_some_and(|source_id| target_history_ids.contains(&source_id))
        }));
        let temp_table_count = store
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master WHERE type = 'table' AND name = ?1",
                params![CANONICAL_FORK_HISTORY_ID_MAP_TABLE],
                |row| row.get::<_, i64>(0),
            )
            .expect("temporary mapping table count");
        assert_eq!(
            temp_table_count, 0,
            "the mapping owner ends with the transaction"
        );
    }

    #[test]
    fn canonical_fork_detects_a_changed_source_fence_and_rolls_back_every_target_row() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        let source_item = history_user_turn(source_session_id, turn_id, 0, 1, "source");
        store
            .seed_history_item_for_test(&source_item)
            .expect("source append");

        let injected_item_id = HistoryItemId::new();
        let injected_payload = serde_json::to_string(&HistoryItemPayload::Error {
            message: "injected source mutation".to_string(),
        })
        .expect("injected payload");
        let injected_payload_hash = hash_text(&injected_payload);
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            locked
                .execute_batch(&format!(
                    "CREATE TRIGGER mutate_source_during_canonical_fork
                     AFTER INSERT ON protocol_history_items
                     WHEN NEW.session_id = '{target_session_id}'
                      AND NOT EXISTS (
                          SELECT 1 FROM protocol_history_items
                          WHERE id = '{injected_item_id}'
                      )
                     BEGIN
                         INSERT INTO protocol_history_items
                             (id, session_id, scope_kind, turn_id, sequence_no, payload_json,
                              payload_sha256, created_at_ms)
                         VALUES
                             ('{injected_item_id}', '{source_session_id}', 'turn', '{turn_id}', 99,
                              '{injected_payload}', '{injected_payload_hash}', 99);
                         INSERT INTO protocol_item_append_order
                             (session_id, scope_kind, turn_id, sequence_no, source_kind, source_id,
                              created_at_ms)
                         VALUES
                             ('{source_session_id}', 'turn', '{turn_id}', 99, 'history_item',
                              '{injected_item_id}', 99);
                     END;"
                ))
                .expect("source mutation trigger");
        }

        let error = store
            .fork_canonical_items(source_session_id, target_session_id)
            .expect_err("the final source fence must reject an in-transaction mutation");
        assert!(
            error
                .to_string()
                .contains("canonical protocol source fence changed")
        );
        let source_after = store
            .list_history_items_for_session(source_session_id)
            .expect("source after rollback");
        assert_eq!(source_after.len(), 1);
        assert_eq!(source_after[0].id, source_item.id);
        let locked = store.connection.lock().expect("sqlite mutex");
        for table in [
            "protocol_history_items",
            "protocol_turn_items",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1");
            let count = locked
                .query_row(&sql, params![target_session_id.to_string()], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("rolled-back target row count");
            assert_eq!(count, 0, "partial canonical fork rows remained in {table}");
        }
        let temp_table_count = locked
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master WHERE type = 'table' AND name = ?1",
                params![CANONICAL_FORK_HISTORY_ID_MAP_TABLE],
                |row| row.get::<_, i64>(0),
            )
            .expect("temporary mapping table count");
        assert_eq!(temp_table_count, 0);
    }

    #[test]
    fn fork_agent_context_copies_only_model_visible_parent_messages() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        let user_turn = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 0,
            created_at_ms: 10,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: "investigate the protocol".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        };
        let assistant = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 2,
            created_at_ms: 30,
            payload: HistoryItemPayload::AssistantMessage {
                response_id: crate::protocol::ModelResponseId::new(),
                content: vec![ContentPart::Text {
                    text: "parent result".to_string(),
                }],
            },
        };
        let communication = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 3,
            created_at_ms: 40,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: crate::protocol::InterAgentCommunication {
                    author: "/root/reviewer".to_string(),
                    recipient: "/root".to_string(),
                    content: "review feedback".to_string(),
                    trigger_turn: false,
                },
            },
        };
        for item in [&user_turn, &assistant, &communication] {
            store
                .seed_history_item_for_test(item)
                .expect("source append");
        }

        let copied = store
            .fork_agent_context(source_session_id, target_session_id)
            .expect("agent context fork");
        let forked = store
            .list_history_items_for_session(target_session_id)
            .expect("forked history");

        assert_eq!(copied, 2);
        assert_eq!(forked.len(), 2);
        assert_eq!(forked[0].session_id, target_session_id);
        assert_eq!(forked[1].session_id, target_session_id);
        assert_ne!(forked[0].id, user_turn.id);
        assert_ne!(forked[1].id, assistant.id);
        assert!(matches!(
            &forked[0].payload,
            HistoryItemPayload::UserTurn {
                content,
                prompt_dispatch: None,
                editor_context: None,
            } if matches!(content.as_slice(), [ContentPart::Text { text }] if text == "investigate the protocol")
        ));
        assert!(matches!(
            &forked[1].payload,
            HistoryItemPayload::AssistantMessage {
                content,
                ..
            } if matches!(content.as_slice(), [ContentPart::Text { text }] if text == "parent result")
        ));
        assert!(
            store
                .list_turn_items_for_session(target_session_id)
                .expect("forked turn items")
                .is_empty()
        );

        store
            .seed_runtime_event_for_test(&warning_event(
                target_session_id,
                turn_id,
                0,
                "after fork",
            ))
            .expect("post-fork append");
        assert_eq!(
            store
                .list_runtime_events(target_session_id, turn_id)
                .expect("events")[0]
                .sequence_no,
            3
        );
    }

    #[test]
    fn fork_agent_context_preserves_compacted_view_without_resurrecting_replaced_items() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        let old_user = history_user_turn(source_session_id, turn_id, 0, 10, "obsolete detail");
        let old_assistant = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 20,
            payload: HistoryItemPayload::AssistantMessage {
                response_id: crate::protocol::ModelResponseId::new(),
                content: vec![ContentPart::Text {
                    text: "obsolete response".to_string(),
                }],
            },
        };
        let compaction = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 2,
            created_at_ms: 30,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "the earlier exchange established the compacted contract".to_string(),
                replacement_item_ids: vec![old_user.id, old_assistant.id],
            },
        };
        let current_user = history_user_turn(
            source_session_id,
            turn_id,
            3,
            40,
            "continue from the compacted contract",
        );
        for item in [&old_user, &old_assistant, &compaction, &current_user] {
            store
                .seed_history_item_for_test(item)
                .expect("source append");
        }

        assert_eq!(
            store
                .fork_agent_context(source_session_id, target_session_id)
                .expect("agent context fork"),
            2
        );
        let forked = store
            .list_history_items_for_session(target_session_id)
            .expect("forked history");

        assert_eq!(forked.len(), 2);
        assert!(matches!(
            &forked[0].payload,
            HistoryItemPayload::Compaction {
                summary,
                replacement_item_ids,
                ..
            } if summary.contains("compacted contract") && replacement_item_ids.is_empty()
        ));
        assert!(matches!(
            &forked[1].payload,
            HistoryItemPayload::UserTurn { content, .. }
                if matches!(content.as_slice(), [ContentPart::Text { text }]
                    if text == "continue from the compacted contract")
        ));
        let messages =
            crate::agent::context_manager::ContextManager::rehydrate(forked).model_messages(false);
        assert!(matches!(
            messages.as_slice(),
            [
                crate::llm::ModelMessage::System { content: summary },
                crate::llm::ModelMessage::User { content: current }
            ] if summary.contains("compacted contract")
                && current == "continue from the compacted contract"
        ));
    }

    #[test]
    fn fork_agent_context_rejects_non_empty_target() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        store
            .seed_history_item_for_test(&history_user_turn(
                source_session_id,
                turn_id,
                0,
                1,
                "source",
            ))
            .expect("source append");
        store
            .seed_runtime_event_for_test(&warning_event(
                target_session_id,
                TurnId::new(),
                0,
                "target",
            ))
            .expect("target append");

        let error = store
            .fork_agent_context(source_session_id, target_session_id)
            .expect_err("non-empty target must be rejected");
        assert!(error.to_string().contains("non-empty target session"));
    }

    #[test]
    fn large_agent_context_fork_streams_active_compacted_history_in_bounded_pages() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        let replaced = (0..100usize)
            .map(|index| {
                history_user_turn(
                    source_session_id,
                    turn_id,
                    i64::try_from(index).unwrap_or(i64::MAX),
                    i64::try_from(index).unwrap_or(i64::MAX),
                    &format!("obsolete detail {index}"),
                )
            })
            .collect::<Vec<_>>();
        let replacement_item_ids = replaced.iter().map(|item| item.id).collect::<Vec<_>>();
        let compaction = HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 100,
            created_at_ms: 100,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                summary: "the obsolete prefix is summarized once".to_string(),
                replacement_item_ids,
            },
        };
        let current = (0..450usize)
            .map(|index| {
                let sequence = index.saturating_add(101);
                history_user_turn(
                    source_session_id,
                    turn_id,
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    &format!("current detail {index}"),
                )
            })
            .collect::<Vec<_>>();
        let mut source_items = replaced;
        source_items.push(compaction);
        source_items.extend(current);
        seed_history_batch_for_test(&store, &source_items);

        let stats = store
            .fork_agent_context_with_stats_for_test(source_session_id, target_session_id, None)
            .expect("bounded agent context fork");
        assert_eq!(stats.source_fence.canonical_count, 551);
        assert_eq!(stats.source_fence.active_count, 451);
        assert_eq!(stats.copied_items, 451);
        assert_eq!(stats.source_pages, 3);
        assert_eq!(stats.max_source_page_items, MAX_PROTOCOL_PAGE_LIMIT);

        let forked = store
            .list_history_items_for_session(target_session_id)
            .expect("forked active history");
        assert_eq!(forked.len(), 451);
        assert!(matches!(
            &forked[0].payload,
            HistoryItemPayload::Compaction {
                summary,
                replacement_item_ids,
                ..
            } if summary == "the obsolete prefix is summarized once"
                && replacement_item_ids.is_empty()
        ));
        assert!(forked.iter().all(|item| !matches!(
            &item.payload,
            HistoryItemPayload::UserTurn { content, .. }
                if matches!(content.as_slice(), [ContentPart::Text { text }]
                    if text.starts_with("obsolete detail"))
        )));
        let temporary_state_count = store
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*)
                 FROM sqlite_temp_master
                 WHERE type = 'table' AND name = ?1",
                params![ACTIVE_HISTORY_TRAVERSAL_TABLE],
                |row| row.get::<_, i64>(0),
            )
            .expect("temporary active-history state count");
        assert_eq!(temporary_state_count, 0);
    }

    #[test]
    fn agent_context_fork_reports_a_typed_stale_fence_without_copying_rows() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        store
            .seed_history_item_for_test(&history_user_turn(
                source_session_id,
                turn_id,
                0,
                1,
                "before fence",
            ))
            .expect("first source item");
        let stale_fence =
            active_history_pages_for_test(&store, source_session_id, MAX_PROTOCOL_PAGE_LIMIT)
                .expect("source fence")
                .0;
        store
            .seed_history_item_for_test(&history_user_turn(
                source_session_id,
                turn_id,
                1,
                2,
                "after fence",
            ))
            .expect("source append after fence");

        let error = store
            .fork_agent_context_with_stats_for_test(
                source_session_id,
                target_session_id,
                Some(stale_fence),
            )
            .expect_err("stale source fence must fail");
        assert!(matches!(
            error,
            StorageError::CanonicalHistoryFenceChanged {
                session_id,
                expected_history_count: 1,
                actual_history_count: 2,
                ..
            } if session_id == source_session_id
        ));
        assert!(
            store
                .list_history_items_for_session(target_session_id)
                .expect("target history")
                .is_empty()
        );
    }

    #[test]
    fn specialized_protocol_fork_rejects_a_missing_target_session_without_orphans() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let missing_target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id]);
        }
        store
            .seed_history_item_for_test(&history_user_turn(
                source_session_id,
                TurnId::new(),
                0,
                1,
                "source",
            ))
            .expect("source append");

        let error = store
            .fork_agent_context(source_session_id, missing_target_session_id)
            .expect_err("missing target must be rejected");
        assert!(error.to_string().contains("missing target session"));
        let canonical_error = store
            .fork_canonical_items(source_session_id, missing_target_session_id)
            .expect_err("canonical fork must also reject a missing target");
        assert!(
            canonical_error
                .to_string()
                .contains("missing target session")
        );
        let locked = store.connection.lock().expect("sqlite mutex");
        for table in [
            "protocol_history_items",
            "protocol_turn_items",
            "protocol_runtime_events",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1");
            let count = locked
                .query_row(
                    &sql,
                    params![missing_target_session_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("orphan row count");
            assert_eq!(count, 0, "orphan rows in {table}");
        }
    }

    #[test]
    fn specialized_protocol_fork_rolls_back_all_rows_after_a_mid_copy_failure() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        let source_items = [
            history_user_turn(source_session_id, turn_id, 0, 1, "first"),
            history_user_turn(source_session_id, turn_id, 1, 2, "second"),
        ];
        seed_history_batch_for_test(&store, &source_items);
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            locked
                .execute_batch(&format!(
                    "CREATE TRIGGER fail_agent_context_fork_after_first
                     BEFORE INSERT ON protocol_history_items
                     WHEN NEW.session_id = '{}'
                       AND (SELECT COUNT(*) FROM protocol_history_items
                            WHERE session_id = NEW.session_id) >= 1
                     BEGIN
                         SELECT RAISE(ABORT, 'injected agent context fork failure');
                     END;",
                    target_session_id
                ))
                .expect("failure trigger");
        }

        let error = store
            .fork_agent_context(source_session_id, target_session_id)
            .expect_err("injected second insert must fail");
        assert!(
            error
                .to_string()
                .contains("injected agent context fork failure")
        );
        let locked = store.connection.lock().expect("sqlite mutex");
        for table in [
            "protocol_history_items",
            "protocol_item_append_order",
            "protocol_turn_sequence_allocators",
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1");
            let count = locked
                .query_row(&sql, params![target_session_id.to_string()], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("rolled-back target row count");
            assert_eq!(count, 0, "partial fork rows remained in {table}");
        }
    }

    #[test]
    fn concurrent_event_appends_claim_distinct_database_sequences() {
        let temp = tempfile::tempdir().expect("tempdir");
        let database_path = temp.path().join("protocol.sqlite3");
        let open_store = |path: &std::path::Path| {
            let connection = Connection::open(path).expect("database");
            connection
                .busy_timeout(Duration::from_secs(5))
                .expect("busy timeout");
            crate::storage::migration::run(&connection).expect("migrations");
            SqliteProtocolEventStore::new(Arc::new(Mutex::new(connection)))
        };
        let first_store = open_store(&database_path);
        let second_store = open_store(&database_path);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let first_event = warning_event(session_id, turn_id, 0, "first");
        let second_event = warning_event(session_id, turn_id, 0, "second");
        let expected_ids = [first_event.id, second_event.id];
        let barrier = Arc::new(Barrier::new(3));

        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_store
                .append_recording_projection_allocating(&first_event, None, None)
                .expect("first append")
                .runtime_event
        });
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_store
                .append_recording_projection_allocating(&second_event, None, None)
                .expect("second append")
                .runtime_event
        });
        barrier.wait();
        let stored = [
            first.join().expect("first worker"),
            second.join().expect("second worker"),
        ];
        let mut sequence_numbers = stored
            .iter()
            .map(|event| event.sequence_no)
            .collect::<Vec<_>>();
        sequence_numbers.sort_unstable();
        assert_eq!(sequence_numbers, vec![0, 1]);
        assert!(
            expected_ids
                .iter()
                .all(|expected| stored.iter().any(|event| event.id == *expected))
        );

        let final_store = open_store(&database_path);
        let persisted = final_store
            .list_runtime_events(session_id, turn_id)
            .expect("persisted events");
        assert_eq!(
            persisted
                .iter()
                .map(|event| event.sequence_no)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            expected_ids
                .iter()
                .all(|expected| persisted.iter().any(|event| event.id == *expected))
        );
    }

    #[test]
    fn test_seed_helpers_share_one_turn_allocator() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let history = history_user_turn(session_id, turn_id, 0, 1, "history");
        let turn = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no: 0,
            payload: TurnItemPayload::AgentMessage {
                text: "turn".to_string(),
            },
        };
        let event = warning_event(session_id, turn_id, 0, "runtime");

        store
            .seed_history_item_for_test(&history)
            .expect("history append");
        store.seed_turn_item_for_test(&turn).expect("turn append");
        store
            .seed_runtime_event_for_test(&event)
            .expect("event append");

        assert_eq!(
            store
                .list_history_items(session_id, turn_id)
                .expect("history")[0]
                .sequence_no,
            0
        );
        assert_eq!(
            store.list_turn_items(session_id, turn_id).expect("turns")[0].sequence_no,
            1
        );
        assert_eq!(
            store
                .list_runtime_events(session_id, turn_id)
                .expect("events")[0]
                .sequence_no,
            2
        );
    }

    #[test]
    fn recording_projection_rejects_atomic_owner_payloads_without_writing_any_stream() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = crate::protocol::ModelResponseId::new();
        let tool_call_id = crate::session::ToolCallId::new();
        let forbidden = vec![
            crate::session::RunEvent::UserTurnStored {
                session_id,
                turn: Box::new(crate::protocol::UserTurn {
                    turn_id,
                    items: vec![crate::protocol::UserInputItem::Text {
                        text: "must use the user-turn owner".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                }),
            },
            crate::session::RunEvent::AssistantMessageCommitted {
                response_id,
                text: "must use the model-response owner".to_string(),
            },
            crate::session::RunEvent::ToolCallPending {
                tool_call_id,
                response_id,
                model_call_id: "provider-call".to_string(),
                tool_name: "read".to_string(),
                arguments_json: "{}".to_string(),
            },
            crate::session::RunEvent::ToolCallCompleted {
                tool_call_id,
                tool: crate::tool::ToolName::Read,
                title: "read".to_string(),
                summary: "must use the tool-settlement owner".to_string(),
                metadata: serde_json::Value::Null,
            },
            crate::session::RunEvent::FileChangesRecorded {
                tool_call_id,
                changes: Vec::new(),
            },
            crate::session::RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                    final_response_id: Some(response_id),
                    tool_call_count: 1,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                }),
            },
        ];

        for event in forbidden {
            let projection =
                crate::protocol::project_protocol_run_event(&event, Some(session_id), turn_id, 0)
                    .expect("forbidden event still has a projection for its atomic owner");
            let error = store
                .append_recording_projection_allocating(
                    &projection.runtime_event,
                    projection.history_item.as_ref(),
                    projection.turn_item.as_ref(),
                )
                .expect_err("recording projection must reject atomic-owner payload");
            assert!(error.to_string().contains("atomic state owner"));
        }

        assert!(
            store
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events")
                .is_empty()
        );
        assert!(
            store
                .list_history_items(session_id, turn_id)
                .expect("history items")
                .is_empty()
        );
        assert!(
            store
                .list_turn_items(session_id, turn_id)
                .expect("turn items")
                .is_empty()
        );
    }

    #[test]
    fn admitted_recording_rejects_a_different_active_turn_owner() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        let active_admission_id = crate::session::AdmissionId::new();
        let session_id = SessionId::new();
        let owned_turn_id = TurnId::new();
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
            let project_id = crate::session::ProjectId::new();
            locked
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES (?1, 'C:/fixture', 'fixture', 'none', 1, 1)",
                    params![project_id.to_string()],
                )
                .expect("project fixture");
            locked
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      access_mode, model_parameters_json, created_at_ms, updated_at_ms,
                      completed_at_ms, active_run_id, active_turn_id,
                      active_run_lease_expires_at_ms)
                     VALUES (?1, ?2, 'fixture', 'running', 'C:/fixture', 'model',
                             'http://localhost', 'default', '{}', 1, 1, NULL,
                             ?3, ?4, 1000)",
                    params![
                        session_id.to_string(),
                        project_id.to_string(),
                        active_admission_id.to_string(),
                        owned_turn_id.to_string()
                    ],
                )
                .expect("current admission fixture");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let turn_id = TurnId::new();
        let event = warning_event(session_id, turn_id, 0, "must have an exact turn owner");

        assert!(
            store
                .append_admitted_recording_projection_allocating_at(
                    active_admission_id,
                    &event,
                    None,
                    None,
                    100,
                )
                .expect("ownership query")
                .is_none(),
            "an admission must not write to a different active turn"
        );
        assert!(
            store
                .list_runtime_events(session_id, turn_id)
                .expect("runtime events")
                .is_empty()
        );

        store
            .seed_runtime_event_for_test(&completed_terminal_event(session_id, owned_turn_id, 0))
            .expect("corrupt running-plus-terminal fixture");
        let corrupt_append = store
            .append_admitted_recording_projection_allocating_at(
                active_admission_id,
                &warning_event(session_id, owned_turn_id, 0, "must reject corruption"),
                None,
                None,
                100,
            )
            .expect_err("running plus terminal must fail closed before recording append");
        assert!(
            corrupt_append
                .to_string()
                .contains("already has a durable terminal")
        );
        assert_eq!(
            store
                .list_runtime_events(session_id, owned_turn_id)
                .expect("owned-turn events after rejected append")
                .len(),
            1
        );
    }

    #[test]
    fn sub_agent_activity_rejects_stale_turn_owners_and_forged_child_identity() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        let root_session_id = SessionId::new();
        let child_session_id = SessionId::new();
        let admission_a = AdmissionId::new();
        let turn_a = TurnId::new();
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
            let project_id = crate::session::ProjectId::new();
            locked
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES (?1, 'C:/activity-fixture', 'fixture', 'none', 1, 1)",
                    params![project_id.to_string()],
                )
                .expect("project fixture");
            locked
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      access_mode, model_parameters_json, created_at_ms, updated_at_ms,
                      completed_at_ms, active_run_id, active_turn_id,
                      active_run_lease_expires_at_ms)
                      VALUES (?1, ?2, 'fixture', 'running', 'C:/activity-fixture', 'model',
                              'http://localhost', 'default', '{}', 1, 1, NULL,
                              ?3, ?4, ?5)",
                    params![
                        root_session_id.to_string(),
                        project_id.to_string(),
                        admission_a.to_string(),
                        turn_a.to_string(),
                        i64::MAX
                    ],
                )
                .expect("active turn fixture");
            locked
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      access_mode, model_parameters_json, created_at_ms, updated_at_ms,
                      completed_at_ms)
                     VALUES (?1, ?2, 'child', 'idle', 'C:/activity-fixture', 'model',
                             'http://localhost', 'default', '{}', 1, 1, NULL)",
                    params![child_session_id.to_string(), project_id.to_string()],
                )
                .expect("child fixture");
            locked
                .execute(
                    "INSERT INTO session_spawn_edges
                     (root_session_id, parent_session_id, child_session_id, agent_path,
                      task_name, created_at_ms)
                     VALUES (?1, ?1, ?2, '/root/reviewer', 'reviewer', 1)",
                    params![root_session_id.to_string(), child_session_id.to_string()],
                )
                .expect("direct-child lineage fixture");
        }
        let store = SqliteProtocolEventStore::new(Arc::clone(&connection));
        store
            .append_sub_agent_activity(
                root_session_id,
                admission_a,
                turn_a,
                "activity-1".to_string(),
                child_session_id,
                "/root/reviewer".to_string(),
                SubAgentActivityKind::Interacted,
            )
            .expect("sub-agent activity append");

        assert!(matches!(
            store
                .list_runtime_events(root_session_id, turn_a)
                .expect("runtime events")
                .as_slice(),
            [RuntimeEvent {
                sequence_no: 0,
                msg: RuntimeEventMsg::SubAgentActivity { .. },
                ..
            }]
        ));
        assert!(matches!(
            store
                .list_history_items(root_session_id, turn_a)
                .expect("history items")[0]
                .payload,
            HistoryItemPayload::SubAgentActivity { .. }
        ));
        assert!(matches!(
            store
                .list_turn_items(root_session_id, turn_a)
                .expect("turn items")[0]
                .payload,
            TurnItemPayload::SubAgentActivity { .. }
        ));

        store
            .seed_runtime_event_for_test(&completed_terminal_event(root_session_id, turn_a, 1))
            .expect("corrupt running-plus-terminal fixture");
        let corrupt_activity = store
            .append_sub_agent_activity(
                root_session_id,
                admission_a,
                turn_a,
                "activity-after-terminal".to_string(),
                child_session_id,
                "/root/reviewer".to_string(),
                SubAgentActivityKind::Interacted,
            )
            .expect_err("running plus terminal must fail closed before activity append");
        assert!(
            corrupt_activity
                .to_string()
                .contains("already has a durable terminal")
        );
        assert_eq!(
            store
                .list_history_items(root_session_id, turn_a)
                .expect("history after rejected corrupt activity")
                .len(),
            1
        );

        let admission_b = AdmissionId::new();
        let turn_b = TurnId::new();
        connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "UPDATE sessions
                 SET status = 'completed', completed_at_ms = 2,
                     active_run_id = NULL, active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1",
                params![root_session_id.to_string()],
            )
            .expect("terminalize root turn A");
        connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "UPDATE sessions
                 SET status = 'running', completed_at_ms = NULL,
                     active_run_id = ?2, active_turn_id = ?3,
                     active_run_lease_expires_at_ms = ?4
                 WHERE id = ?1",
                params![
                    root_session_id.to_string(),
                    admission_b.to_string(),
                    turn_b.to_string(),
                    i64::MAX
                ],
            )
            .expect("admit replacement root turn");
        let stale_error = store
            .append_sub_agent_activity(
                root_session_id,
                admission_a,
                turn_a,
                "delayed-activity-a".to_string(),
                child_session_id,
                "/root/reviewer".to_string(),
                SubAgentActivityKind::Interrupted,
            )
            .expect_err("turn A activity must not attach to admitted turn B");
        assert!(stale_error.to_string().contains("stale"));

        let forged_error = store
            .append_sub_agent_activity(
                root_session_id,
                admission_b,
                turn_b,
                "forged-path".to_string(),
                child_session_id,
                "/root/forged".to_string(),
                SubAgentActivityKind::Interacted,
            )
            .expect_err("forged child path must be rejected");
        assert!(forged_error.to_string().contains("stale"));
        let forged_session_error = store
            .append_sub_agent_activity(
                root_session_id,
                admission_b,
                turn_b,
                "forged-session".to_string(),
                SessionId::new(),
                "/root/reviewer".to_string(),
                SubAgentActivityKind::Interacted,
            )
            .expect_err("forged child session must be rejected");
        assert!(forged_session_error.to_string().contains("stale"));
        assert_eq!(
            store
                .list_history_items(root_session_id, turn_a)
                .expect("turn A history after stale append")
                .len(),
            1
        );
        assert!(
            store
                .list_history_items(root_session_id, turn_b)
                .expect("turn B history after rejected appends")
                .is_empty()
        );
    }

    #[test]
    fn fork_seeds_allocator_after_copied_maximum_sequence() {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("in-memory db"),
        ));
        {
            let locked = connection.lock().expect("sqlite mutex");
            crate::storage::migration::run(&locked).expect("migrations");
        }
        let store = SqliteProtocolEventStore::new(connection);
        let source_session_id = SessionId::new();
        let target_session_id = SessionId::new();
        {
            let locked = store.connection.lock().expect("sqlite mutex");
            seed_fork_sessions(&locked, &[source_session_id, target_session_id]);
        }
        let turn_id = TurnId::new();
        store
            .seed_history_item_for_test(&history_user_turn(
                source_session_id,
                turn_id,
                29,
                1,
                "source",
            ))
            .expect("source append");
        store
            .fork_canonical_items(source_session_id, target_session_id)
            .expect("fork");
        store
            .seed_runtime_event_for_test(&warning_event(
                target_session_id,
                turn_id,
                0,
                "after fork",
            ))
            .expect("post-fork append");

        assert_eq!(
            store
                .list_runtime_events(target_session_id, turn_id)
                .expect("events")[0]
                .sequence_no,
            30
        );
    }

    fn warning_event(
        session_id: SessionId,
        turn_id: TurnId,
        sequence_no: i64,
        message: &str,
    ) -> RuntimeEvent {
        RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: 1,
            msg: RuntimeEventMsg::Warning {
                message: message.to_string(),
            },
        }
    }

    fn completed_terminal_event(
        session_id: SessionId,
        turn_id: TurnId,
        sequence_no: i64,
    ) -> RuntimeEvent {
        crate::protocol::project_protocol_run_event(
            &crate::session::RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                }),
            },
            Some(session_id),
            turn_id,
            sequence_no,
        )
        .expect("terminal projection")
        .runtime_event
    }

    fn history_user_turn(
        session_id: SessionId,
        turn_id: TurnId,
        sequence_no: i64,
        created_at_ms: i64,
        text: &str,
    ) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no,
            created_at_ms,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: text.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        }
    }
}
