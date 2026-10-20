use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::SqliteSessionRepository;
use crate::error::StorageError;
use crate::protocol::{HistoryItemId, ToolLifecycleStatus, TurnId};
use crate::session::{SessionId, ToolCallId};

pub(crate) const MAX_SECURITY_REVIEW_CHECKS: usize = 32;
pub(crate) const MAX_SECURITY_REVIEW_CHECK_REFERENCE_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SecurityReviewState {
    pub latest_effect_position: Option<i64>,
    pub latest_effect_item_id: Option<HistoryItemId>,
    pub latest_review_position: Option<i64>,
    pub reviewed_effect_position: Option<i64>,
    pub reviewed_effect_item_id: Option<HistoryItemId>,
    pub check_count: usize,
    pub successful_check_count: usize,
    pub finding_count: usize,
    pub limitation_count: usize,
    pub unclassified_tool_count: usize,
}

impl SecurityReviewState {
    /// Freshness is an observed history relationship, not a safety verdict.
    pub(crate) fn is_current(&self) -> bool {
        self.latest_review_position.is_some_and(|review| {
            self.unclassified_tool_count == 0
                && self.reviewed_effect_item_id == self.latest_effect_item_id
                && self
                    .latest_effect_position
                    .is_none_or(|effect| review > effect)
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SecurityReviewCheck {
    pub call_id: ToolCallId,
    pub history_item_id: HistoryItemId,
    pub append_position: i64,
    pub tool_name: String,
    pub status: ToolLifecycleStatus,
    pub success: Option<bool>,
    pub security_effect: Option<String>,
}

impl SecurityReviewCheck {
    /// This establishes only a successful referenced operation and ordering.
    /// It does not establish that the operation adequately tested security.
    pub(crate) fn is_successful_current_check(&self, effect_position: Option<i64>) -> bool {
        self.status == ToolLifecycleStatus::Completed
            && self.success == Some(true)
            && effect_position.is_none_or(|effect| self.append_position >= effect)
            && matches!(
                self.security_effect.as_deref(),
                Some("read" | "mutation" | "destructive")
            )
            && !matches!(
                self.tool_name.as_str(),
                "current_time"
                    | "skill"
                    | "update_plan"
                    | "compact_context"
                    | "get_goal"
                    | "create_goal"
                    | "update_goal"
                    | "spawn_agent"
                    | "send_message"
                    | "followup_task"
                    | "wait_agent"
                    | "interrupt_agent"
                    | "list_agents"
            )
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SecurityReviewEvidence {
    pub state: SecurityReviewState,
    pub checks: Vec<SecurityReviewCheck>,
}

impl SqliteSessionRepository {
    pub(crate) fn security_review_state(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<SecurityReviewState, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let state = state_in_connection(&transaction, session_id, turn_id)?;
        transaction.commit()?;
        Ok(state)
    }

    /// Resolve only bounded identifiers and outcome metadata. Tool output text
    /// is deliberately not loaded or reinterpreted as evidence of success.
    pub(crate) fn security_review_evidence(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        check_references: &[String],
    ) -> Result<SecurityReviewEvidence, StorageError> {
        if check_references.len() > MAX_SECURITY_REVIEW_CHECKS
            || check_references.iter().any(|reference| {
                reference.is_empty() || reference.len() > MAX_SECURITY_REVIEW_CHECK_REFERENCE_BYTES
            })
        {
            return Err(invalid(
                "security review check references exceed their bounds",
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let state = state_in_connection(&transaction, session_id, turn_id)?;
        let mut seen = HashSet::new();
        let mut checks = Vec::with_capacity(check_references.len());
        for reference in check_references {
            let check = resolve_check(&transaction, session_id, turn_id, reference)?;
            if !seen.insert(check.call_id) {
                return Err(invalid(
                    "security review contains duplicate canonical check IDs",
                ));
            }
            checks.push(check);
        }
        transaction.commit()?;
        Ok(SecurityReviewEvidence { state, checks })
    }
}

fn invalid(message: &str) -> StorageError {
    StorageError::Message(message.into())
}

fn state_in_connection(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<SecurityReviewState, StorageError> {
    let (latest_effect_position, unclassified_tool_count): (Option<i64>, i64) = connection.query_row(
        "SELECT MAX(CASE
                    WHEN json_extract(h.payload_json,'$.kind')='file_change'
                      OR (json_extract(h.payload_json,'$.kind')='tool_output'
                          AND json_extract(h.payload_json,'$.status')<>'declined'
                          AND json_extract(h.payload_json,'$.metadata.security_effect') IN ('mutation','destructive'))
                    THEN a.append_position END),
                COALESCE(SUM(CASE
                    WHEN json_extract(h.payload_json,'$.kind')='tool_output'
                      AND json_extract(h.payload_json,'$.status')<>'declined'
                      AND COALESCE(json_extract(h.payload_json,'$.metadata.security_effect'),'')
                          NOT IN ('read','mutation','destructive')
                    THEN 1 ELSE 0 END),0)
         FROM protocol_history_items h
         JOIN protocol_item_append_order a
           ON a.source_kind='history_item' AND a.source_id=h.id AND a.session_id=h.session_id
         WHERE h.session_id=?1 AND h.scope_kind='turn' AND h.turn_id=?2",
        params![session_id.to_string(), turn_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let latest_effect_item_id = latest_effect_position
        .map(|position| {
            let id: String = connection.query_row(
                "SELECT source_id FROM protocol_item_append_order
             WHERE append_position=?1 AND source_kind='history_item' AND session_id=?2",
                params![position, session_id.to_string()],
                |row| row.get(0),
            )?;
            id.parse()
                .map_err(|_| invalid("invalid effect history item ID"))
        })
        .transpose()?;
    type ReviewRow = (
        i64,
        Option<i64>,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );
    let review: Option<ReviewRow> = connection
        .query_row(
            "SELECT a.append_position,
                    json_extract(h.payload_json,'$.metadata.tool_metadata.reviewed_effect_position'),
                    json_extract(h.payload_json,'$.metadata.tool_metadata.reviewed_effect_item_id'),
                    json_extract(h.payload_json,'$.metadata.tool_metadata.check_count'),
                    json_extract(h.payload_json,'$.metadata.tool_metadata.successful_check_count'),
                    json_extract(h.payload_json,'$.metadata.tool_metadata.finding_count'),
                    json_extract(h.payload_json,'$.metadata.tool_metadata.limitation_count')
             FROM protocol_history_items h
             JOIN protocol_item_append_order a
               ON a.source_kind='history_item' AND a.source_id=h.id AND a.session_id=h.session_id
             WHERE h.session_id=?1 AND h.scope_kind='turn' AND h.turn_id=?2
               AND json_extract(h.payload_json,'$.kind')='tool_output'
               AND json_extract(h.payload_json,'$.status')='completed'
               AND json_extract(h.payload_json,'$.success')=1
               AND json_type(h.payload_json,'$.metadata.tool_metadata.reviewed_effect_item_id')
                   IN ('text','null')
               AND EXISTS (
                 SELECT 1 FROM protocol_history_items c
                 WHERE c.session_id=h.session_id AND c.scope_kind='turn' AND c.turn_id=h.turn_id
                   AND json_extract(c.payload_json,'$.kind')='tool_call'
                   AND json_extract(c.payload_json,'$.call_id')=json_extract(h.payload_json,'$.call_id')
                   AND json_extract(c.payload_json,'$.tool_name')='security_review')
             ORDER BY a.append_position DESC LIMIT 1",
            params![session_id.to_string(), turn_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .optional()?;
    let (
        latest_review_position,
        reviewed_effect_position,
        reviewed_effect_item_id,
        check_count,
        successful_check_count,
        finding_count,
        limitation_count,
    ) = match review {
        Some((position, reviewed, reviewed_id, count, successful, findings, limitations)) => (
            Some(position),
            reviewed,
            reviewed_id
                .map(|id| {
                    id.parse()
                        .map_err(|_| invalid("invalid reviewed effect item ID"))
                })
                .transpose()?,
            usize::try_from(count.unwrap_or(0))
                .map_err(|_| invalid("invalid review check count"))?,
            usize::try_from(successful.unwrap_or(0))
                .map_err(|_| invalid("invalid successful check count"))?,
            usize::try_from(findings.unwrap_or(0))
                .map_err(|_| invalid("invalid review finding count"))?,
            usize::try_from(limitations.unwrap_or(0))
                .map_err(|_| invalid("invalid review limitation count"))?,
        ),
        None => (None, None, None, 0, 0, 0, 0),
    };
    Ok(SecurityReviewState {
        latest_effect_position,
        latest_effect_item_id,
        latest_review_position,
        reviewed_effect_position,
        reviewed_effect_item_id,
        check_count,
        successful_check_count,
        finding_count,
        limitation_count,
        unclassified_tool_count: usize::try_from(unclassified_tool_count)
            .map_err(|_| invalid("invalid unclassified tool count"))?,
    })
}

fn resolve_check(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
    reference: &str,
) -> Result<SecurityReviewCheck, StorageError> {
    let mut statement = connection.prepare(
        "SELECT json_extract(c.payload_json,'$.call_id'),json_extract(c.payload_json,'$.tool_name')
         FROM protocol_history_items c
         WHERE c.session_id=?1 AND c.scope_kind='turn' AND c.turn_id=?2
           AND json_extract(c.payload_json,'$.kind')='tool_call'
           AND (json_extract(c.payload_json,'$.call_id')=?3
                OR json_extract(c.payload_json,'$.model_call_id')=?3)
         LIMIT 2",
    )?;
    let calls = statement
        .query_map(
            params![session_id.to_string(), turn_id.to_string(), reference],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let [(call_id, tool_name)] = calls.as_slice() else {
        return Err(invalid(
            "check ID must identify exactly one tool call in the current turn",
        ));
    };
    if tool_name == "security_review" {
        return Err(invalid("security_review cannot cite itself as a check"));
    }
    if tool_name.len() > 256 {
        return Err(invalid("check tool name exceeds the review evidence bound"));
    }
    let mut statement = connection.prepare(
        "SELECT h.id,a.append_position,json_extract(h.payload_json,'$.status'),
                json_extract(h.payload_json,'$.success'),
                json_extract(h.payload_json,'$.metadata.security_effect')
         FROM protocol_history_items h
         JOIN protocol_item_append_order a
           ON a.source_kind='history_item' AND a.source_id=h.id AND a.session_id=h.session_id
         WHERE h.session_id=?1 AND h.scope_kind='turn' AND h.turn_id=?2
           AND json_extract(h.payload_json,'$.kind')='tool_output'
           AND json_extract(h.payload_json,'$.call_id')=?3
           AND json_extract(h.payload_json,'$.status') NOT IN ('pending','running')
         LIMIT 2",
    )?;
    let outputs = statement
        .query_map(
            params![session_id.to_string(), turn_id.to_string(), call_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<bool>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let [(history_item_id, append_position, status, success, security_effect)] = outputs.as_slice()
    else {
        return Err(invalid(
            "check ID must have exactly one settled output in the current turn",
        ));
    };
    Ok(SecurityReviewCheck {
        call_id: call_id
            .parse()
            .map_err(|_| invalid("invalid canonical tool call ID"))?,
        history_item_id: history_item_id
            .parse()
            .map_err(|_| invalid("invalid history item ID"))?,
        append_position: *append_position,
        tool_name: tool_name.clone(),
        status: serde_json::from_value(serde_json::Value::String(status.clone()))?,
        success: *success,
        security_effect: security_effect.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use serde_json::{Value, json};

    use crate::protocol::{
        CompactionLayout, CompactionMode, HistoryItem, HistoryItemPayload, HistoryScope,
        ModelResponseId, ProtocolEventStore,
    };
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    fn fixture() -> (tempfile::TempDir, StoreBundle, SessionId, TurnId) {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
        let sqlite = SqliteStore::open(&StoragePaths {
            database_path: data_dir.join("review.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        })
        .unwrap();
        sqlite.migrate().unwrap();
        let store = StoreBundle::new(sqlite);
        let session = SessionId::new();
        store
            .protocol_event_store()
            .seed_session_for_test(session)
            .unwrap();
        (temp, store, session, TurnId::new())
    }

    fn append(
        store: &StoreBundle,
        session: SessionId,
        turn: TurnId,
        payload: HistoryItemPayload,
    ) -> i64 {
        let id = HistoryItemId::new();
        store
            .protocol_event_store()
            .seed_history_item_for_test(&HistoryItem {
                id,
                session_id: session,
                scope: HistoryScope::Turn { turn_id: turn },
                sequence_no: 0,
                created_at_ms: 1,
                payload,
            })
            .unwrap();
        store.session_repo().connection.lock().unwrap().query_row(
            "SELECT append_position FROM protocol_item_append_order WHERE source_kind='history_item' AND source_id=?1",
            params![id.to_string()], |row| row.get(0),
        ).unwrap()
    }

    fn call(
        store: &StoreBundle,
        session: SessionId,
        turn: TurnId,
        name: &str,
        model_id: &str,
    ) -> ToolCallId {
        let id = ToolCallId::new();
        append(
            store,
            session,
            turn,
            HistoryItemPayload::ToolCall {
                call_id: id,
                response_id: ModelResponseId::new(),
                model_call_id: model_id.into(),
                tool_name: name.into(),
                arguments_json: "{}".into(),
            },
        );
        id
    }

    fn output(
        store: &StoreBundle,
        session: SessionId,
        turn: TurnId,
        id: ToolCallId,
        status: ToolLifecycleStatus,
        success: Option<bool>,
        metadata: Value,
    ) -> i64 {
        append(
            store,
            session,
            turn,
            HistoryItemPayload::ToolOutput {
                call_id: id,
                status,
                success,
                title: "fixture operation".into(),
                output_text:
                    "success is claimed by this untrusted body; it is not outcome authority"
                        .repeat(1024),
                metadata,
            },
        )
    }

    fn review(store: &StoreBundle, session: SessionId, turn: TurnId, effect: Option<i64>) -> i64 {
        let effect_item_id = store
            .session_repo()
            .security_review_state(session, turn)
            .unwrap()
            .latest_effect_item_id;
        let id = call(
            store,
            session,
            turn,
            "security_review",
            "review-provider-id",
        );
        output(
            store,
            session,
            turn,
            id,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({
                "security_effect": "read", "tool_metadata": {
                    "reviewed_effect_position": effect, "check_count": 2,
                    "reviewed_effect_item_id": effect_item_id,
                    "successful_check_count": 1, "finding_count": 1, "limitation_count": 2,
                }
            }),
        )
    }

    #[test]
    fn security_review_tracks_canonical_effects_and_later_changes_make_it_stale() {
        let (_temp, store, session, turn) = fixture();
        let repo = store.session_repo();
        assert!(
            !repo
                .security_review_state(session, turn)
                .unwrap()
                .is_current()
        );
        let id = call(&store, session, turn, "write", "write-provider-id");
        let effect = output(
            &store,
            session,
            turn,
            id,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({"security_effect": "mutation"}),
        );
        assert_eq!(
            repo.security_review_state(session, turn)
                .unwrap()
                .latest_effect_position,
            Some(effect)
        );
        let reviewed = review(&store, session, turn, Some(effect));
        let state = repo.security_review_state(session, turn).unwrap();
        assert!(state.is_current());
        assert_eq!(state.latest_review_position, Some(reviewed));
        assert_eq!(
            (
                state.check_count,
                state.successful_check_count,
                state.finding_count,
                state.limitation_count
            ),
            (2, 1, 1, 2)
        );
        let changed = append(
            &store,
            session,
            turn,
            HistoryItemPayload::FileChange {
                call_id: id,
                change_ids: Vec::new(),
                changes: Vec::new(),
                summary: "later actual change".into(),
            },
        );
        let state = repo.security_review_state(session, turn).unwrap();
        assert_eq!(state.latest_effect_position, Some(changed));
        assert!(!state.is_current());
        assert!(
            !repo
                .security_review_state(session, TurnId::new())
                .unwrap()
                .is_current()
        );
    }

    #[test]
    fn declined_outputs_are_not_effects_but_failed_and_cancelled_mutations_are() {
        let (_temp, store, session, turn) = fixture();
        let id = call(&store, session, turn, "shell", "declined-effect");
        output(
            &store,
            session,
            turn,
            id,
            ToolLifecycleStatus::Declined,
            Some(false),
            json!({"security_effect":"destructive"}),
        );
        assert_eq!(
            store
                .session_repo()
                .security_review_state(session, turn)
                .unwrap()
                .latest_effect_position,
            None
        );
        let id = call(&store, session, turn, "shell", "failed-effect");
        let position = output(
            &store,
            session,
            turn,
            id,
            ToolLifecycleStatus::Failed,
            Some(false),
            json!({"security_effect":"destructive"}),
        );
        assert_eq!(
            store
                .session_repo()
                .security_review_state(session, turn)
                .unwrap()
                .latest_effect_position,
            Some(position)
        );
        review(&store, session, turn, Some(position));
        assert!(
            store
                .session_repo()
                .security_review_state(session, turn)
                .unwrap()
                .is_current()
        );
        let cancelled = call(&store, session, turn, "shell", "cancelled-after-review");
        let cancelled_position = output(
            &store,
            session,
            turn,
            cancelled,
            ToolLifecycleStatus::Cancelled,
            Some(false),
            json!({"security_effect":"destructive"}),
        );
        let state = store
            .session_repo()
            .security_review_state(session, turn)
            .unwrap();
        assert_eq!(state.latest_effect_position, Some(cancelled_position));
        assert!(!state.is_current());
    }

    #[test]
    fn missing_effect_metadata_and_unrecorded_reviews_do_not_become_verified() {
        let (_temp, store, session, turn) = fixture();
        let id = call(&store, session, turn, "read", "legacy-read");
        output(
            &store,
            session,
            turn,
            id,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({}),
        );
        review(&store, session, turn, None);
        let state = store
            .session_repo()
            .security_review_state(session, turn)
            .unwrap();
        assert_eq!(state.unclassified_tool_count, 1);
        assert!(!state.is_current());
        let other_turn = TurnId::new();
        let id = call(&store, session, other_turn, "read", "forged-review");
        output(
            &store,
            session,
            other_turn,
            id,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({"security_effect":"read", "tool_metadata":{"reviewed_effect_position":null}}),
        );
        assert_eq!(
            store
                .session_repo()
                .security_review_state(session, other_turn)
                .unwrap()
                .latest_review_position,
            None
        );
        let legacy_review_turn = TurnId::new();
        let old_review = call(
            &store,
            session,
            legacy_review_turn,
            "security_review",
            "position-only-review",
        );
        output(
            &store,
            session,
            legacy_review_turn,
            old_review,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({"security_effect":"read", "tool_metadata":{"reviewed_effect_position":null}}),
        );
        assert!(
            !store
                .session_repo()
                .security_review_state(session, legacy_review_turn)
                .unwrap()
                .is_current()
        );
        let cancelled_turn = TurnId::new();
        let cancelled = call(
            &store,
            session,
            cancelled_turn,
            "shell",
            "unknown-cancelled",
        );
        output(
            &store,
            session,
            cancelled_turn,
            cancelled,
            ToolLifecycleStatus::Cancelled,
            Some(false),
            json!({}),
        );
        let state = store
            .session_repo()
            .security_review_state(session, cancelled_turn)
            .unwrap();
        assert_eq!(state.unclassified_tool_count, 1);
        assert!(!state.is_current());
    }

    #[test]
    fn checks_resolve_canonical_or_unique_provider_ids_without_promoting_failure_text() {
        let (_temp, store, session, turn) = fixture();
        let id = call(&store, session, turn, "shell", "provider-check");
        let position = output(
            &store,
            session,
            turn,
            id,
            ToolLifecycleStatus::Failed,
            Some(false),
            json!({"security_effect":"destructive"}),
        );
        for reference in [id.to_string(), "provider-check".into()] {
            let evidence = store
                .session_repo()
                .security_review_evidence(session, turn, &[reference])
                .unwrap();
            assert_eq!(evidence.checks[0].call_id, id);
            assert_eq!(evidence.checks[0].append_position, position);
            assert_eq!(evidence.checks[0].success, Some(false));
            assert!(!evidence.checks[0].is_successful_current_check(Some(position)));
            assert!(
                !serde_json::to_string(&evidence)
                    .unwrap()
                    .contains("untrusted body")
            );
        }
        assert!(
            store
                .session_repo()
                .security_review_evidence(session, turn, &[id.to_string(), "provider-check".into()])
                .is_err()
        );
        assert!(
            store
                .session_repo()
                .security_review_evidence(SessionId::new(), turn, &[id.to_string()])
                .is_err()
        );
        assert!(
            store
                .session_repo()
                .security_review_evidence(session, TurnId::new(), &[id.to_string()])
                .is_err()
        );
    }

    #[test]
    fn checks_reject_ambiguous_pending_and_self_references() {
        let (_temp, store, session, turn) = fixture();
        let repo = store.session_repo();
        for _ in 0..2 {
            call(&store, session, turn, "read", "ambiguous");
        }
        assert!(
            repo.security_review_evidence(session, turn, &["ambiguous".into()])
                .is_err()
        );
        let missing_output = call(&store, session, turn, "read", "not-settled");
        assert!(
            repo.security_review_evidence(session, turn, &[missing_output.to_string()])
                .is_err()
        );
        for status in [ToolLifecycleStatus::Pending, ToolLifecycleStatus::Running] {
            let id = call(
                &store,
                session,
                turn,
                "read",
                &ToolCallId::new().to_string(),
            );
            output(
                &store,
                session,
                turn,
                id,
                status,
                None,
                json!({"security_effect":"read"}),
            );
            assert!(
                repo.security_review_evidence(session, turn, &[id.to_string()])
                    .is_err()
            );
        }
        review(&store, session, turn, None);
        assert!(
            repo.security_review_evidence(session, turn, &["review-provider-id".into()])
                .is_err()
        );
        assert!(
            repo.security_review_evidence(session, turn, &vec!["id".into(); 33])
                .is_err()
        );
        assert!(
            repo.security_review_evidence(session, turn, &["x".repeat(129)])
                .is_err()
        );
    }

    #[test]
    fn successful_checks_require_order_and_operation_evidence_not_bookkeeping() {
        let (_temp, store, session, turn) = fixture();
        for (name, effect, expected) in [
            ("shell", "destructive", true),
            ("read", "read", true),
            ("skill", "read", false),
            ("current_time", "read", false),
            ("update_plan", "read", false),
        ] {
            let id = call(&store, session, turn, name, &ToolCallId::new().to_string());
            let position = output(
                &store,
                session,
                turn,
                id,
                ToolLifecycleStatus::Completed,
                Some(true),
                json!({"security_effect":effect}),
            );
            let evidence = store
                .session_repo()
                .security_review_evidence(session, turn, &[id.to_string()])
                .unwrap();
            assert_eq!(
                evidence.checks[0].is_successful_current_check(Some(position)),
                expected
            );
            assert!(!evidence.checks[0].is_successful_current_check(Some(position + 1)));
        }
    }

    #[test]
    fn compaction_and_reopen_keep_review_freshness_in_canonical_history() {
        let (_temp, store, session, turn) = fixture();
        let id = call(&store, session, turn, "write", "write-before-compaction");
        let effect = output(
            &store,
            session,
            turn,
            id,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({"security_effect":"mutation"}),
        );
        review(&store, session, turn, Some(effect));
        let before = store
            .session_repo()
            .security_review_state(session, turn)
            .unwrap();
        let replacements = store
            .protocol_event_store()
            .list_history_items(session, turn)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        append(
            &store,
            session,
            turn,
            HistoryItemPayload::Compaction {
                mode: CompactionMode::Automatic,
                layout: CompactionLayout::UserAnchoredCheckpoint,
                preserved_user_messages: vec!["Continue the task".into()],
                clm_checkpoint: None,
                summary: "A historical summary does not determine the review state".into(),
                replacement_item_ids: replacements,
            },
        );
        let reopened = SqliteStore::open(store.paths()).unwrap();
        assert_eq!(
            reopened
                .session_repo()
                .security_review_state(session, turn)
                .unwrap(),
            before
        );
        assert!(before.is_current());
        for reference in [id.to_string(), "write-before-compaction".into()] {
            let evidence = reopened
                .session_repo()
                .security_review_evidence(session, turn, &[reference])
                .unwrap();
            assert_eq!(evidence.checks[0].call_id, id);
            assert_eq!(evidence.checks[0].append_position, effect);
            assert!(evidence.checks[0].is_successful_current_check(Some(effect)));
        }
    }

    #[tokio::test]
    async fn shared_archive_restore_keeps_stable_review_binding_when_positions_change() {
        use crate::agent::shared::{
            SharedProgress, SharedResume, SharedRunContext, SharedYieldProposal,
        };
        use crate::session::ProjectRepository;
        use crate::storage::session_repo::{ModelResponseWrite, PendingToolCallWrite};

        let (_source_temp, source, session, turn) = fixture();
        let repo = source.session_repo();
        let admission = repo
            .admit_session_turn(session, turn)
            .await
            .unwrap()
            .unwrap()
            .admission_id;
        let context = SharedRunContext {
            job_id: "security-review-archive".into(),
            attempt_id: "source-attempt".into(),
            generation: 1,
            project_id: "portable-project".into(),
            environment_id: "source".into(),
            project_context: None,
            allowed_child_environments: vec!["worker".into()],
            allowed_child_candidates: Vec::new(),
            resume: None,
            continuation: None,
        };
        repo.bind_shared_run(&context, session, turn, admission)
            .unwrap();
        let id = call(&source, session, turn, "write", "archived-write");
        let effect = output(
            &source,
            session,
            turn,
            id,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({"security_effect":"mutation"}),
        );
        review(&source, session, turn, Some(effect));
        let before = repo.security_review_state(session, turn).unwrap();
        assert!(before.is_current());

        // Use the maintained checkpoint/archive lifecycle, including a real
        // pending child sidecar, rather than rewriting a paused history digest.
        let pending_child = ToolCallId::new();
        let child_args = json!({"environment_id":"worker", "title":"Check child", "prompt":"Inspect the delegated data"});
        repo.record_model_response_with_protocol_bundle(
            session,
            admission,
            turn,
            ModelResponseWrite {
                response_id: ModelResponseId::new(),
                assistant_text: None,
                assistant_protocol_sequence_no: None,
                tool_calls: vec![PendingToolCallWrite {
                    id: pending_child,
                    model_call_id: "archive-child".into(),
                    tool_name: "shared_delegate".into(),
                    arguments_json: child_args.to_string(),
                    protocol_sequence_no: None,
                }],
            },
        )
        .await
        .unwrap();
        let yielded = repo
            .checkpoint_shared_run(
                &context,
                session,
                turn,
                admission,
                SharedYieldProposal {
                    tool_call_id: pending_child,
                    child: crate::tool::shared_delegate::parse_child(
                        child_args,
                        &context.allowed_child_environments,
                    )
                    .unwrap(),
                    progress: SharedProgress::default(),
                    retained_service: None,
                },
            )
            .unwrap();
        let archive = repo
            .export_shared_archive(&context.job_id, source.paths())
            .unwrap()
            .unwrap();

        let (_target_temp, target, unrelated, unrelated_turn) = fixture();
        for index in 0..12 {
            let id = call(
                &target,
                unrelated,
                unrelated_turn,
                "read",
                &format!("unrelated-{index}"),
            );
            output(
                &target,
                unrelated,
                unrelated_turn,
                id,
                ToolLifecycleStatus::Completed,
                Some(true),
                json!({"security_effect":"read"}),
            );
        }
        let config = crate::config::ResolvedConfig::default();
        let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
            &target.paths().data_dir,
            &config,
        )
        .unwrap();
        target
            .project_repo()
            .upsert_project(
                workspace.project_id,
                &workspace.root,
                "restore target",
                "none",
            )
            .await
            .unwrap();
        let resumed = SharedRunContext {
            attempt_id: "target-attempt".into(),
            generation: 2,
            resume: Some(SharedResume {
                checkpoint: yielded.checkpoint,
                child_result: json!({}),
                archive: None,
            }),
            ..context
        };
        assert!(
            target
                .session_repo()
                .restore_shared_archive(&resumed, &archive, &workspace, &config, target.paths())
                .unwrap()
        );
        let restored = target
            .session_repo()
            .security_review_state(session, turn)
            .unwrap();
        assert_ne!(
            restored.latest_effect_position,
            before.latest_effect_position
        );
        assert_eq!(restored.latest_effect_item_id, before.latest_effect_item_id);
        assert_eq!(
            restored.reviewed_effect_item_id,
            before.reviewed_effect_item_id
        );
        assert_eq!(
            restored.reviewed_effect_position,
            before.reviewed_effect_position
        );
        assert!(
            restored.is_current(),
            "transport-local append positions do not invalidate the same reviewed effect"
        );
        for reference in [id.to_string(), "archived-write".into()] {
            assert_eq!(
                target
                    .session_repo()
                    .security_review_evidence(session, turn, &[reference])
                    .unwrap()
                    .checks[0]
                    .call_id,
                id
            );
        }
        let changed = call(&target, session, turn, "shell", "post-restore-effect");
        output(
            &target,
            session,
            turn,
            changed,
            ToolLifecycleStatus::Completed,
            Some(true),
            json!({"security_effect":"destructive"}),
        );
        assert!(
            !target
                .session_repo()
                .security_review_state(session, turn)
                .unwrap()
                .is_current()
        );
    }
}
