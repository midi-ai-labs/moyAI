use crate::session::{CanonicalSessionRead, CanonicalTurnPage, SessionRecord};
use crate::tui::state::{AppState, RunStatus};

use super::models::DesktopSessionDetail;
use super::query::{build_session_detail, build_session_detail_from_app_state_with_session};

#[derive(Debug, Clone)]
pub struct OpenSessionView {
    read: CanonicalSessionRead,
    stored_detail: DesktopSessionDetail,
}

impl OpenSessionView {
    pub fn from_loaded(read: &CanonicalSessionRead) -> Self {
        let stored_detail = build_session_detail(read, None);
        Self {
            read: read.clone(),
            stored_detail,
        }
    }

    pub fn session(&self) -> &SessionRecord {
        &self.read.session
    }

    pub fn session_id(&self) -> crate::session::SessionId {
        self.read.session.id
    }

    pub fn turn_items(&self) -> &[crate::protocol::TurnItem] {
        &self.read.turns.items
    }

    pub fn loaded_turn_end(&self) -> usize {
        self.read
            .turns
            .offset
            .saturating_add(self.read.turns.items.len())
    }

    pub fn active_turn_id(&self) -> Option<crate::protocol::TurnId> {
        self.read.active_turn_id
    }

    pub fn merge_contiguous(&mut self, incoming: &CanonicalSessionRead) -> bool {
        if self.session_id() != incoming.session.id
            || incoming.session.id != incoming.turns.session.id
        {
            return false;
        }
        let Some(turns) = merge_contiguous_turn_pages(&self.read.turns, &incoming.turns) else {
            return false;
        };
        let mut read = if canonical_metadata_is_newer(&self.read, incoming) {
            incoming.clone()
        } else {
            self.read.clone()
        };
        read.turns = turns;
        read.turns.session = read.session.clone();
        self.stored_detail = build_session_detail(&read, None);
        self.read = read;
        true
    }

    pub fn refresh_metadata_preserving_loaded_history(
        &mut self,
        incoming: &CanonicalSessionRead,
    ) -> bool {
        if self.session_id() != incoming.session.id
            || incoming.session.id != incoming.turns.session.id
        {
            return false;
        }
        self.read.session = incoming.session.clone();
        self.read.history = incoming.history.clone();
        self.read.latest_turn_id = incoming.latest_turn_id;
        self.read.active_turn_id = incoming.active_turn_id;
        self.read.active_turn_sequence_no = incoming.active_turn_sequence_no;
        self.read.turns.session = incoming.turns.session.clone();
        self.read.turns.limit = self.read.turns.limit.max(incoming.turns.limit);
        self.read.turns.total = self.read.turns.total.max(incoming.turns.total);
        self.read.turns.has_more = self.loaded_turn_end() < self.read.turns.total;
        self.stored_detail = build_session_detail(&self.read, None);
        true
    }

    pub fn live_detail(
        &self,
        app_state: &AppState,
        fallback_snapshot_detail: Option<&DesktopSessionDetail>,
    ) -> DesktopSessionDetail {
        let mut detail =
            build_session_detail_from_app_state_with_session(app_state, Some(&self.read.session));
        if matches!(app_state.run_status, RunStatus::Running) {
            detail.transcript_rows = merge_canonical_prefix_with_live_suffix(
                self.canonical_prefix_rows(),
                detail.transcript_rows,
            );
            detail.thread_empty = detail.transcript_rows.is_empty();
            detail.transcript_text = transcript_text_from_rows(&detail.transcript_rows);
        }
        if !matches!(app_state.run_status, RunStatus::Running)
            && stored_lifecycle_matches_live(self.read.session.status, app_state.run_status)
            && stored_transcript_covers_live_conversation(&self.stored_detail, &detail)
        {
            detail.thread_empty = self.stored_detail.thread_empty;
            detail.transcript_text = self.stored_detail.transcript_text.clone();
            detail.transcript_rows = self.stored_detail.transcript_rows.clone();
        }
        detail.turn_page_offset = self.stored_detail.turn_page_offset;
        detail.turn_page_limit = self.stored_detail.turn_page_limit;
        detail.turn_page_total = self.stored_detail.turn_page_total;
        detail.turn_page_has_more = self.stored_detail.turn_page_has_more;
        if detail.artifacts.is_empty() {
            let fallback = fallback_snapshot_detail.unwrap_or(&self.stored_detail);
            detail.artifacts = fallback.artifacts.clone();
            detail.file_changes = fallback.file_changes.clone();
            detail.file_change_summary_text = fallback.file_change_summary_text.clone();
            detail.artifact_preview_text = fallback.artifact_preview_text.clone();
        }
        detail.artifact_preview_available = !detail.artifacts.is_empty();
        detail
    }

    fn canonical_prefix_rows(&self) -> Vec<super::models::DesktopTranscriptRow> {
        let Some(active_turn_id) = self.read.active_turn_id else {
            return self.stored_detail.transcript_rows.clone();
        };
        let (prior_items, active_items): (Vec<_>, Vec<_>) = self
            .read
            .turns
            .items
            .iter()
            .cloned()
            .partition(|item| item.turn_id != active_turn_id);
        let mut rows = super::query::transcript_rows_from_turn_items_with_context(
            &self.read.session,
            &prior_items,
        );
        rows.extend(
            super::query::transcript_rows_from_turn_items_with_context(
                &self.read.session,
                &active_items,
            )
            .into_iter()
            .filter(|row| {
                matches!(
                    row.row_kind,
                    super::models::DesktopTranscriptRowKind::User
                        | super::models::DesktopTranscriptRowKind::SubAgentStarted
                        | super::models::DesktopTranscriptRowKind::SubAgentUpdated
                        | super::models::DesktopTranscriptRowKind::SubAgentInterrupted
                )
            }),
        );
        rows
    }

    pub fn stored_detail(&self) -> &DesktopSessionDetail {
        &self.stored_detail
    }
}

fn merge_canonical_prefix_with_live_suffix(
    mut prefix: Vec<super::models::DesktopTranscriptRow>,
    live: Vec<super::models::DesktopTranscriptRow>,
) -> Vec<super::models::DesktopTranscriptRow> {
    use super::models::DesktopTranscriptRowKind::{Assistant, EmptyPlaceholder, Error};

    prefix.retain(|row| row.row_kind != EmptyPlaceholder);
    let expected = primary_conversation_rows_from_rows(&prefix);
    let suffix_start =
        canonical_suffix_boundary(&expected, &live).map_or(0, |index| index.saturating_add(1));

    let current_work_summary = live
        .iter()
        .rev()
        .find(|row| is_work_summary_kind(row.row_kind))
        .cloned();
    let mut suffix = live
        .into_iter()
        .skip(suffix_start)
        .filter(|row| !is_work_summary_kind(row.row_kind))
        .collect::<Vec<_>>();
    if let Some(work_summary) = current_work_summary {
        let insert_at = suffix
            .iter()
            .position(|row| matches!(row.row_kind, Assistant | Error))
            .unwrap_or(suffix.len());
        suffix.insert(insert_at, work_summary);
    }
    prefix.extend(suffix);
    prefix
}

fn canonical_suffix_boundary(
    expected: &[(super::models::DesktopTranscriptRowKind, String)],
    live: &[super::models::DesktopTranscriptRow],
) -> Option<usize> {
    use super::models::DesktopTranscriptRowKind::{Assistant, Error, User};

    let live_primary = live
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row.row_kind, User | Assistant | Error))
        .collect::<Vec<_>>();
    for expected_start in 0..expected.len() {
        let mut expected_index = expected_start;
        let mut boundary = None;
        let mut invalid = false;
        for (live_index, row) in &live_primary {
            let Some((expected_kind, expected_body)) = expected.get(expected_index) else {
                break;
            };
            if row.row_kind == *expected_kind && row.body == *expected_body {
                expected_index += 1;
                boundary = Some(*live_index);
                if expected_index == expected.len() {
                    break;
                }
            } else if row.row_kind != Assistant {
                invalid = true;
                break;
            }
        }
        if !invalid && expected_index == expected.len() {
            return boundary;
        }
    }
    None
}

fn primary_conversation_rows_from_rows(
    transcript_rows: &[super::models::DesktopTranscriptRow],
) -> Vec<(super::models::DesktopTranscriptRowKind, String)> {
    primary_conversation_rows_with_indices(transcript_rows)
        .into_iter()
        .map(|(kind, body, _)| (kind, body))
        .collect()
}

fn primary_conversation_rows_with_indices(
    transcript_rows: &[super::models::DesktopTranscriptRow],
) -> Vec<(super::models::DesktopTranscriptRowKind, String, usize)> {
    use super::models::DesktopTranscriptRowKind::{Assistant, Error, User};

    let mut rows = Vec::new();
    let mut user: Option<(String, usize)> = None;
    let mut errors: Vec<(String, usize)> = Vec::new();
    let mut final_assistant: Option<(String, usize)> = None;
    let flush = |rows: &mut Vec<_>,
                 user: &mut Option<(String, usize)>,
                 errors: &mut Vec<(String, usize)>,
                 final_assistant: &mut Option<(String, usize)>| {
        if let Some((body, index)) = user.take() {
            rows.push((User, body, index));
        }
        rows.extend(errors.drain(..).map(|(body, index)| (Error, body, index)));
        if let Some((body, index)) = final_assistant.take() {
            rows.push((Assistant, body, index));
        }
    };

    for (index, row) in transcript_rows.iter().enumerate() {
        match row.row_kind {
            User => {
                flush(&mut rows, &mut user, &mut errors, &mut final_assistant);
                user = Some((row.body.clone(), index));
            }
            Assistant if !row.body.trim().is_empty() => {
                final_assistant = Some((row.body.clone(), index));
            }
            Error => errors.push((row.body.clone(), index)),
            _ => {}
        }
    }
    flush(&mut rows, &mut user, &mut errors, &mut final_assistant);
    rows
}

fn is_work_summary_kind(kind: super::models::DesktopTranscriptRowKind) -> bool {
    use super::models::DesktopTranscriptRowKind::{
        WorkSummaryCancelled, WorkSummaryCompleted, WorkSummaryFailed, WorkSummaryIncomplete,
        WorkSummaryRunning,
    };
    matches!(
        kind,
        WorkSummaryRunning
            | WorkSummaryIncomplete
            | WorkSummaryCompleted
            | WorkSummaryFailed
            | WorkSummaryCancelled
    )
}

fn transcript_text_from_rows(rows: &[super::models::DesktopTranscriptRow]) -> String {
    rows.iter()
        .map(|row| {
            if row.body.trim().is_empty() {
                row.title.clone()
            } else {
                format!("{}\n{}", row.title, row.body.trim())
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn stored_transcript_covers_live_conversation(
    stored: &DesktopSessionDetail,
    live: &DesktopSessionDetail,
) -> bool {
    let stored_rows = primary_conversation_rows(stored);
    let live_rows = primary_conversation_rows(live);
    stored_rows.ends_with(&live_rows)
}

fn stored_lifecycle_matches_live(stored: crate::session::SessionStatus, live: RunStatus) -> bool {
    matches!(
        (stored, live),
        (crate::session::SessionStatus::Idle, RunStatus::Idle)
            | (crate::session::SessionStatus::Running, RunStatus::Running)
            | (
                crate::session::SessionStatus::Completed,
                RunStatus::Completed
            )
            | (
                crate::session::SessionStatus::Cancelled,
                RunStatus::Cancelled
            )
            | (crate::session::SessionStatus::Failed, RunStatus::Failed)
    )
}

fn primary_conversation_rows(
    detail: &DesktopSessionDetail,
) -> Vec<(super::models::DesktopTranscriptRowKind, String)> {
    use super::models::DesktopTranscriptRowKind::{Assistant, Error, User};

    let mut rows = Vec::new();
    let mut user: Option<String> = None;
    let mut errors: Vec<String> = Vec::new();
    let mut final_assistant: Option<String> = None;
    let flush = |rows: &mut Vec<_>,
                 user: &mut Option<String>,
                 errors: &mut Vec<String>,
                 final_assistant: &mut Option<String>| {
        if let Some(body) = user.take() {
            rows.push((User, body));
        }
        rows.extend(errors.drain(..).map(|body| (Error, body)));
        if let Some(body) = final_assistant.take() {
            rows.push((Assistant, body));
        }
    };

    for row in &detail.transcript_rows {
        match row.row_kind {
            User => {
                flush(&mut rows, &mut user, &mut errors, &mut final_assistant);
                user = Some(row.body.clone());
            }
            Assistant if !row.body.trim().is_empty() => {
                final_assistant = Some(row.body.clone());
            }
            Error => errors.push(row.body.clone()),
            _ => {}
        }
    }
    flush(&mut rows, &mut user, &mut errors, &mut final_assistant);
    rows
}

fn merge_contiguous_turn_pages(
    existing: &CanonicalTurnPage,
    incoming: &CanonicalTurnPage,
) -> Option<CanonicalTurnPage> {
    if existing.session.id != incoming.session.id {
        return None;
    }
    let existing_end = existing.offset.checked_add(existing.items.len())?;
    let incoming_end = incoming.offset.checked_add(incoming.items.len())?;
    if incoming.offset > existing_end || existing.offset > incoming_end {
        return None;
    }

    let overlap_start = existing.offset.max(incoming.offset);
    let overlap_end = existing_end.min(incoming_end);
    for position in overlap_start..overlap_end {
        let existing_item = &existing.items[position - existing.offset];
        let incoming_item = &incoming.items[position - incoming.offset];
        if existing_item.id != incoming_item.id {
            return None;
        }
    }

    let merged_offset = existing.offset.min(incoming.offset);
    let merged_end = existing_end.max(incoming_end);
    let mut merged = vec![None; merged_end.saturating_sub(merged_offset)];
    for (index, item) in existing.items.iter().cloned().enumerate() {
        merged[existing.offset - merged_offset + index] = Some(item);
    }
    for (index, item) in incoming.items.iter().cloned().enumerate() {
        merged[incoming.offset - merged_offset + index] = Some(item);
    }
    let items = merged.into_iter().collect::<Option<Vec<_>>>()?;
    let total = existing.total.max(incoming.total).max(merged_end);
    Some(CanonicalTurnPage {
        session: incoming.session.clone(),
        offset: merged_offset,
        limit: existing.limit.max(incoming.limit),
        total,
        has_more: merged_end < total,
        items,
    })
}

fn canonical_metadata_is_newer(
    existing: &CanonicalSessionRead,
    incoming: &CanonicalSessionRead,
) -> bool {
    if incoming.session.updated_at_ms != existing.session.updated_at_ms {
        return incoming.session.updated_at_ms > existing.session.updated_at_ms;
    }
    session_status_rank(incoming.session.status) > session_status_rank(existing.session.status)
}

const fn session_status_rank(status: crate::session::SessionStatus) -> u8 {
    match status {
        crate::session::SessionStatus::Idle => 0,
        crate::session::SessionStatus::Running => 1,
        crate::session::SessionStatus::Completed
        | crate::session::SessionStatus::Cancelled
        | crate::session::SessionStatus::Failed => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::models::{
        DesktopArtifactRow, DesktopTranscriptRow, DesktopTranscriptRowKind,
    };
    use crate::protocol::{TurnId, TurnItem, TurnItemId, TurnItemPayload, TurnTerminalOutcome};
    use crate::session::{
        CanonicalHistoryPage, CanonicalTurnPage, ProjectId, SessionId, SessionStatus,
    };
    use crate::tui::state::{TranscriptEntry, TranscriptKind};

    fn session() -> SessionRecord {
        SessionRecord {
            id: SessionId::new(),
            project_id: ProjectId::new(),
            title: "session".to_string(),
            status: SessionStatus::Completed,
            cwd: "C:/workspace".into(),
            model: "model".to_string(),
            base_url: "http://127.0.0.1:1234".to_string(),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: Default::default(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: Some(2),
        }
    }

    fn canonical_read(
        session: &SessionRecord,
        offset: usize,
        limit: usize,
        total: usize,
        items: Vec<TurnItem>,
    ) -> CanonicalSessionRead {
        CanonicalSessionRead {
            session: session.clone(),
            history: CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: 0,
                total: 0,
                has_more: false,
                items: Vec::new(),
            },
            turns: CanonicalTurnPage {
                session: session.clone(),
                offset,
                limit,
                total,
                has_more: offset.saturating_add(items.len()) < total,
                items,
            },
            latest_turn_id: None,
            active_turn_id: None,
            active_turn_sequence_no: None,
        }
    }

    fn turn_item(
        session_id: SessionId,
        turn_id: TurnId,
        sequence_no: i64,
        payload: TurnItemPayload,
    ) -> TurnItem {
        TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no,
            payload,
        }
    }

    fn transcript_row(
        row_kind: DesktopTranscriptRowKind,
        title: &str,
        body: &str,
    ) -> DesktopTranscriptRow {
        DesktopTranscriptRow {
            row_kind,
            step: String::new(),
            title: title.to_string(),
            body: body.to_string(),
            file_changes: Vec::new(),
        }
    }

    #[test]
    fn artifact_fallback_does_not_replace_a_fresh_live_transcript() {
        let session = session();
        let mut stored_detail =
            build_session_detail_from_app_state_with_session(&AppState::default(), Some(&session));
        stored_detail.transcript_text = "stale transcript".to_string();
        stored_detail.transcript_rows = vec![DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::FileChanges,
            step: "1".to_string(),
            title: "old changes".to_string(),
            body: "stale transcript".to_string(),
            file_changes: Vec::new(),
        }];
        stored_detail.artifacts = vec![DesktopArtifactRow {
            label: "result.txt".to_string(),
            path: "result.txt".to_string(),
            kind: "file".to_string(),
            action: "update".to_string(),
        }];
        let view = OpenSessionView {
            read: canonical_read(&session, 0, 0, 0, Vec::new()),
            stored_detail,
        };
        let mut live = AppState::default();
        live.transcript_entries = vec![TranscriptEntry {
            kind: TranscriptKind::Assistant,
            title: "Assistant".to_string(),
            body: "fresh terminal answer".to_string(),
            response_id: None,
            tool_call_id: None,
        }];

        let detail = view.live_detail(&live, None);

        assert!(detail.transcript_text.contains("fresh terminal answer"));
        assert!(!detail.transcript_text.contains("stale transcript"));
        assert!(detail.artifact_preview_available);
        assert_eq!(detail.artifacts.len(), 1);
    }

    #[test]
    fn running_detail_keeps_the_canonical_previous_turn_and_adds_only_the_live_suffix() {
        let session = session();
        let mut stored_detail =
            build_session_detail_from_app_state_with_session(&AppState::default(), Some(&session));
        stored_detail.thread_empty = false;
        stored_detail.transcript_rows = vec![
            transcript_row(
                DesktopTranscriptRowKind::User,
                "ユーザー依頼",
                "first request",
            ),
            transcript_row(
                DesktopTranscriptRowKind::SubAgentStarted,
                "/root/first_agent",
                "",
            ),
            transcript_row(
                DesktopTranscriptRowKind::SubAgentUpdated,
                "/root/first_agent",
                "done",
            ),
            transcript_row(
                DesktopTranscriptRowKind::WorkSummaryCompleted,
                "first history",
                "first work",
            ),
            transcript_row(DesktopTranscriptRowKind::Assistant, "応答", "first final"),
        ];
        let view = OpenSessionView {
            read: canonical_read(&session, 0, 0, 0, Vec::new()),
            stored_detail,
        };
        let mut live = AppState::default();
        live.current_session_id = Some(session.id);
        live.run_status = RunStatus::Running;
        live.transcript_entries = vec![
            TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "first request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "first final".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "second request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "second progress".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let detail = view.live_detail(&live, None);
        let kinds = detail
            .transcript_rows
            .iter()
            .map(|row| row.row_kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
                .count(),
            1
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == DesktopTranscriptRowKind::WorkSummaryRunning)
                .count(),
            1
        );
        assert!(kinds.contains(&DesktopTranscriptRowKind::SubAgentStarted));
        let first_final = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "first final")
            .expect("stored final");
        let second_user = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "second request")
            .expect("live user");
        assert!(first_final < second_user);
    }

    #[test]
    fn running_goal_continuation_places_live_work_after_the_canonical_final() {
        let session = session();
        let mut stored_detail =
            build_session_detail_from_app_state_with_session(&AppState::default(), Some(&session));
        stored_detail.thread_empty = false;
        stored_detail.transcript_rows = vec![
            transcript_row(
                DesktopTranscriptRowKind::User,
                "ユーザー依頼",
                "first request",
            ),
            transcript_row(
                DesktopTranscriptRowKind::WorkSummaryCompleted,
                "first history",
                "first work",
            ),
            transcript_row(DesktopTranscriptRowKind::Assistant, "応答", "first final"),
        ];
        let view = OpenSessionView {
            read: canonical_read(&session, 0, 0, 0, Vec::new()),
            stored_detail,
        };
        let mut live = AppState::default();
        live.current_session_id = Some(session.id);
        live.run_status = RunStatus::Running;
        live.transcript_entries = vec![
            TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "first request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "first final".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "continuation progress".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let detail = view.live_detail(&live, None);
        assert_eq!(
            detail
                .transcript_rows
                .iter()
                .filter(|row| row.body == "first final")
                .count(),
            1
        );
        let first_final = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "first final")
            .expect("stored final");
        let running = detail
            .transcript_rows
            .iter()
            .position(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning)
            .expect("live work summary");
        let continuation = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "continuation progress")
            .expect("live continuation");
        assert!(first_final < running && running < continuation);
    }

    #[test]
    fn active_same_turn_prepend_projects_earlier_agent_history_with_the_live_suffix() {
        let mut session = session();
        session.status = SessionStatus::Running;
        session.completed_at_ms = None;
        let active_turn = TurnId::new();
        let items = vec![
            turn_item(
                session.id,
                active_turn,
                1,
                TurnItemPayload::UserMessage {
                    text: "active request".to_string(),
                },
            ),
            turn_item(
                session.id,
                active_turn,
                2,
                TurnItemPayload::SubAgentActivity {
                    activity_id: "spawn-reviewer".to_string(),
                    agent_session_id: SessionId::new(),
                    agent_path: "/root/reviewer".to_string(),
                    activity_kind: crate::protocol::SubAgentActivityKind::Started,
                },
            ),
        ];
        let mut suffix = canonical_read(&session, 1, 1, items.len(), items[1..].to_vec());
        suffix.active_turn_id = Some(active_turn);
        let mut earlier = canonical_read(&session, 0, items.len(), items.len(), items);
        earlier.active_turn_id = Some(active_turn);
        let mut view = OpenSessionView::from_loaded(&suffix);
        assert!(view.merge_contiguous(&earlier));

        let mut live = AppState::default();
        live.current_session_id = Some(session.id);
        live.run_status = RunStatus::Running;
        live.transcript_entries = vec![
            TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "active request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "current progress".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let detail = view.live_detail(&live, None);
        assert_eq!(
            detail
                .transcript_rows
                .iter()
                .filter(|row| row.body == "active request")
                .count(),
            1
        );
        assert_eq!(
            detail
                .transcript_rows
                .iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::SubAgentStarted)
                .count(),
            1
        );
        assert_eq!(
            detail
                .transcript_rows
                .iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning)
                .count(),
            1
        );
        assert_eq!(
            detail
                .transcript_rows
                .iter()
                .filter(|row| row.body == "current progress")
                .count(),
            1
        );
        let agent = detail
            .transcript_rows
            .iter()
            .position(|row| row.row_kind == DesktopTranscriptRowKind::SubAgentStarted)
            .expect("prepended Sub Agent history");
        let running = detail
            .transcript_rows
            .iter()
            .position(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning)
            .expect("live work summary");
        let progress = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "current progress")
            .expect("live progress");
        assert!(agent < running && running < progress);
    }

    #[test]
    fn prepended_prior_turns_absent_from_live_state_do_not_hide_the_current_turn() {
        let mut session = session();
        session.status = SessionStatus::Running;
        session.completed_at_ms = None;
        let prior_turn = TurnId::new();
        let active_turn = TurnId::new();
        let items = vec![
            turn_item(
                session.id,
                prior_turn,
                1,
                TurnItemPayload::UserMessage {
                    text: "older request".to_string(),
                },
            ),
            turn_item(
                session.id,
                prior_turn,
                2,
                TurnItemPayload::AgentMessage {
                    text: "older final".to_string(),
                },
            ),
            turn_item(
                session.id,
                prior_turn,
                3,
                TurnItemPayload::Terminal {
                    outcome: TurnTerminalOutcome::Completed,
                },
            ),
            turn_item(
                session.id,
                active_turn,
                4,
                TurnItemPayload::UserMessage {
                    text: "current request".to_string(),
                },
            ),
            turn_item(
                session.id,
                active_turn,
                5,
                TurnItemPayload::SubAgentActivity {
                    activity_id: "spawn-current".to_string(),
                    agent_session_id: SessionId::new(),
                    agent_path: "/root/current_reviewer".to_string(),
                    activity_kind: crate::protocol::SubAgentActivityKind::Started,
                },
            ),
        ];
        let mut read = canonical_read(&session, 0, items.len(), items.len(), items);
        read.active_turn_id = Some(active_turn);
        let view = OpenSessionView::from_loaded(&read);
        let mut live = AppState::default();
        live.current_session_id = Some(session.id);
        live.run_status = RunStatus::Running;
        live.transcript_entries = vec![
            TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "current request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "current progress".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let detail = view.live_detail(&live, None);
        for body in [
            "older request",
            "older final",
            "current request",
            "current progress",
        ] {
            assert_eq!(
                detail
                    .transcript_rows
                    .iter()
                    .filter(|row| row.body == body)
                    .count(),
                1,
                "{body} remains visible exactly once",
            );
        }
        let older_final = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "older final")
            .expect("older final");
        let current_user = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "current request")
            .expect("current user");
        let current_agent = detail
            .transcript_rows
            .iter()
            .position(|row| row.row_kind == DesktopTranscriptRowKind::SubAgentStarted)
            .expect("current Sub Agent history");
        let current_summary = detail
            .transcript_rows
            .iter()
            .position(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning)
            .expect("current work summary");
        let current_progress = detail
            .transcript_rows
            .iter()
            .position(|row| row.body == "current progress")
            .expect("current progress");
        assert!(older_final < current_user);
        assert!(current_user < current_agent);
        assert!(current_agent < current_summary);
        assert!(current_summary < current_progress);
    }

    #[test]
    fn canonical_overlap_is_a_contiguous_suffix_prefix_and_keeps_intervening_live_rows() {
        let expected = vec![
            (DesktopTranscriptRowKind::User, "same request".to_string()),
            (
                DesktopTranscriptRowKind::Assistant,
                "same answer".to_string(),
            ),
        ];
        let live = vec![
            transcript_row(
                DesktopTranscriptRowKind::User,
                "ユーザー依頼",
                "same request",
            ),
            transcript_row(
                DesktopTranscriptRowKind::Error,
                "エラー",
                "intervening live error",
            ),
            transcript_row(DesktopTranscriptRowKind::Assistant, "応答", "same answer"),
        ];

        assert_eq!(canonical_suffix_boundary(&expected, &live), None);

        let repeated_turns = vec![
            (DesktopTranscriptRowKind::User, "same request".to_string()),
            (
                DesktopTranscriptRowKind::Assistant,
                "same answer".to_string(),
            ),
            (DesktopTranscriptRowKind::User, "same request".to_string()),
        ];
        assert_eq!(
            canonical_suffix_boundary(&repeated_turns, &live[..1]),
            Some(0)
        );
    }

    #[test]
    fn terminal_live_detail_uses_canonical_turn_boundaries_when_the_read_is_fresh() {
        let session = session();
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let items = vec![
            turn_item(
                session.id,
                first_turn,
                1,
                TurnItemPayload::UserMessage {
                    text: "first request".to_string(),
                },
            ),
            turn_item(
                session.id,
                first_turn,
                2,
                TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "verify".to_string(),
                    summary: "passed".to_string(),
                },
            ),
            turn_item(
                session.id,
                first_turn,
                3,
                TurnItemPayload::AgentMessage {
                    text: "first final answer".to_string(),
                },
            ),
            turn_item(
                session.id,
                first_turn,
                4,
                TurnItemPayload::Terminal {
                    outcome: TurnTerminalOutcome::Completed,
                },
            ),
            turn_item(
                session.id,
                second_turn,
                1,
                TurnItemPayload::UserMessage {
                    text: "second request".to_string(),
                },
            ),
            turn_item(
                session.id,
                second_turn,
                2,
                TurnItemPayload::AgentMessage {
                    text: "second final answer".to_string(),
                },
            ),
            turn_item(
                session.id,
                second_turn,
                3,
                TurnItemPayload::Terminal {
                    outcome: TurnTerminalOutcome::Completed,
                },
            ),
        ];
        let view = OpenSessionView::from_loaded(&canonical_read(
            &session,
            0,
            items.len(),
            items.len(),
            items.clone(),
        ));
        let mut live = AppState::default();
        live.load_turn_items(&session, &items);

        let detail = view.live_detail(&live, None);
        let row_index = |body: &str| {
            detail
                .transcript_rows
                .iter()
                .position(|row| row.body == body)
                .expect("expected transcript row")
        };
        let work_summary_indices = detail
            .transcript_rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                (row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted).then_some(index)
            })
            .collect::<Vec<_>>();

        assert_eq!(work_summary_indices.len(), 2);
        assert!(row_index("first request") < work_summary_indices[0]);
        assert!(work_summary_indices[0] < row_index("first final answer"));
        assert!(row_index("first final answer") < row_index("second request"));
        assert!(row_index("second request") < work_summary_indices[1]);
        assert!(work_summary_indices[1] < row_index("second final answer"));
    }

    #[test]
    fn terminal_live_detail_folds_no_tool_intermediate_assistant_rows_after_reopen() {
        let session = session();
        let turn_id = TurnId::new();
        let items = vec![
            turn_item(
                session.id,
                turn_id,
                1,
                TurnItemPayload::UserMessage {
                    text: "request without tools".to_string(),
                },
            ),
            turn_item(
                session.id,
                turn_id,
                2,
                TurnItemPayload::AgentMessage {
                    text: "intermediate progress".to_string(),
                },
            ),
            turn_item(
                session.id,
                turn_id,
                3,
                TurnItemPayload::AgentMessage {
                    text: "final response".to_string(),
                },
            ),
            turn_item(
                session.id,
                turn_id,
                4,
                TurnItemPayload::Terminal {
                    outcome: TurnTerminalOutcome::Completed,
                },
            ),
        ];
        let view = OpenSessionView::from_loaded(&canonical_read(
            &session,
            0,
            items.len(),
            items.len(),
            items.clone(),
        ));
        let mut live = AppState::default();
        live.load_turn_items(&session, &items);
        let uncorrected = build_session_detail_from_app_state_with_session(&live, Some(&session));
        assert!(
            uncorrected
                .transcript_rows
                .iter()
                .any(|row| row.body == "intermediate progress")
        );

        let detail = view.live_detail(&live, None);

        assert!(
            !detail
                .transcript_rows
                .iter()
                .any(|row| row.body == "intermediate progress")
        );
        assert!(
            detail
                .transcript_rows
                .iter()
                .any(|row| row.body == "final response")
        );
        assert_eq!(
            detail
                .transcript_rows
                .iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
                .count(),
            1
        );
    }

    #[test]
    fn terminal_live_detail_rejects_a_stale_running_canonical_lifecycle() {
        let mut running_session = session();
        running_session.status = SessionStatus::Running;
        running_session.completed_at_ms = None;
        let turn_id = TurnId::new();
        let items = vec![
            turn_item(
                running_session.id,
                turn_id,
                1,
                TurnItemPayload::UserMessage {
                    text: "request before stop".to_string(),
                },
            ),
            turn_item(
                running_session.id,
                turn_id,
                2,
                TurnItemPayload::AgentMessage {
                    text: "response before stop".to_string(),
                },
            ),
        ];
        let view = OpenSessionView::from_loaded(&canonical_read(
            &running_session,
            0,
            items.len(),
            items.len(),
            items.clone(),
        ));
        assert!(
            view.stored_detail()
                .transcript_rows
                .iter()
                .any(|row| { row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning })
        );
        let mut live = AppState::default();
        live.load_turn_items(&running_session, &items);
        live.run_status = RunStatus::Cancelled;

        let detail = view.live_detail(&live, None);

        assert!(
            !detail
                .transcript_rows
                .iter()
                .any(|row| { row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning })
        );
    }

    #[test]
    fn terminal_live_detail_keeps_live_rows_when_the_canonical_suffix_differs() {
        let session = session();
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let items = vec![
            turn_item(
                session.id,
                first_turn,
                1,
                TurnItemPayload::UserMessage {
                    text: "stored first request".to_string(),
                },
            ),
            turn_item(
                session.id,
                first_turn,
                2,
                TurnItemPayload::AgentMessage {
                    text: "shared first answer".to_string(),
                },
            ),
            turn_item(
                session.id,
                second_turn,
                1,
                TurnItemPayload::UserMessage {
                    text: "shared last request".to_string(),
                },
            ),
            turn_item(
                session.id,
                second_turn,
                2,
                TurnItemPayload::AgentMessage {
                    text: "shared last answer".to_string(),
                },
            ),
        ];
        let view = OpenSessionView::from_loaded(&canonical_read(
            &session,
            0,
            items.len(),
            items.len(),
            items,
        ));
        let mut live = AppState::default();
        live.current_session_id = Some(session.id);
        live.run_status = RunStatus::Completed;
        live.transcript_entries = vec![
            TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "live different first request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "shared first answer".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "shared last request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "shared last answer".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let detail = view.live_detail(&live, None);

        assert!(
            detail
                .transcript_rows
                .iter()
                .any(|row| row.body == "live different first request")
        );
        assert!(
            !detail
                .transcript_rows
                .iter()
                .any(|row| row.body == "stored first request")
        );
    }

    #[test]
    fn earlier_chunk_reprojects_a_turn_split_across_the_page_boundary() {
        let session = session();
        let turn_id = TurnId::new();
        let items = vec![
            turn_item(
                session.id,
                turn_id,
                1,
                TurnItemPayload::UserMessage {
                    text: "old request".to_string(),
                },
            ),
            turn_item(
                session.id,
                turn_id,
                2,
                TurnItemPayload::AgentMessage {
                    text: "final answer".to_string(),
                },
            ),
            turn_item(
                session.id,
                turn_id,
                3,
                TurnItemPayload::Terminal {
                    outcome: TurnTerminalOutcome::Completed,
                },
            ),
        ];
        let suffix = canonical_read(&session, 1, 2, 3, items[1..].to_vec());
        let earlier = canonical_read(&session, 0, 2, 3, items[..2].to_vec());
        let mut view = OpenSessionView::from_loaded(&suffix);

        assert!(view.merge_contiguous(&earlier));

        assert_eq!(view.read.turns.offset, 0);
        assert_eq!(view.read.turns.items.len(), 3);
        assert!(!view.read.turns.has_more);
        let rows = &view.stored_detail().transcript_rows;
        assert_eq!(
            rows.iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::User)
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| { row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted })
                .count(),
            1
        );
        assert!(rows.iter().any(|row| row.body.contains("old request")));
        assert!(rows.iter().any(|row| row.body.contains("final answer")));
    }

    #[test]
    fn overlapping_latest_refresh_keeps_the_expanded_earlier_suffix() {
        let session = session();
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let items = vec![
            turn_item(
                session.id,
                first_turn,
                1,
                TurnItemPayload::UserMessage {
                    text: "retained old request".to_string(),
                },
            ),
            turn_item(
                session.id,
                first_turn,
                2,
                TurnItemPayload::AgentMessage {
                    text: "retained old answer".to_string(),
                },
            ),
            turn_item(
                session.id,
                second_turn,
                3,
                TurnItemPayload::UserMessage {
                    text: "new request".to_string(),
                },
            ),
            turn_item(
                session.id,
                second_turn,
                4,
                TurnItemPayload::AgentMessage {
                    text: "new answer".to_string(),
                },
            ),
            turn_item(
                session.id,
                second_turn,
                5,
                TurnItemPayload::Terminal {
                    outcome: TurnTerminalOutcome::Completed,
                },
            ),
        ];
        let expanded = canonical_read(&session, 0, 2, 4, items[..4].to_vec());
        let latest = canonical_read(&session, 3, 2, 5, items[3..].to_vec());
        let mut view = OpenSessionView::from_loaded(&expanded);

        assert!(view.merge_contiguous(&latest));

        assert_eq!(view.read.turns.offset, 0);
        assert_eq!(view.read.turns.total, 5);
        assert_eq!(view.read.turns.items.len(), 5);
        assert_eq!(view.read.turns.items[0].id, items[0].id);
        assert_eq!(view.read.turns.items[4].id, items[4].id);
        let rows = &view.stored_detail().transcript_rows;
        assert!(
            rows.iter()
                .any(|row| row.body.contains("retained old request"))
        );
        assert!(rows.iter().any(|row| row.body.contains("new answer")));
    }

    #[test]
    fn stale_prepend_total_merges_by_overlap_without_shrinking_the_live_total() {
        let session = session();
        let turn_id = TurnId::new();
        let items = (0..5)
            .map(|index| {
                turn_item(
                    session.id,
                    turn_id,
                    index + 1,
                    TurnItemPayload::AgentMessage {
                        text: format!("item {index}"),
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut view =
            OpenSessionView::from_loaded(&canonical_read(&session, 2, 3, 5, items[2..].to_vec()));
        let stale_prepend = canonical_read(&session, 0, 4, 4, items[..4].to_vec());

        assert!(view.merge_contiguous(&stale_prepend));
        assert_eq!(view.read.turns.offset, 0);
        assert_eq!(view.read.turns.items.len(), 5);
        assert_eq!(view.read.turns.total, 5);
        assert!(!view.read.turns.has_more);
    }

    #[test]
    fn noncontiguous_live_refresh_preserves_the_loaded_canonical_prefix() {
        let session = session();
        let turn_id = TurnId::new();
        let prefix_items = vec![turn_item(
            session.id,
            turn_id,
            1,
            TurnItemPayload::UserMessage {
                text: "retained prefix".to_string(),
            },
        )];
        let suffix_items = vec![turn_item(
            session.id,
            turn_id,
            82,
            TurnItemPayload::AgentMessage {
                text: "noncontiguous suffix".to_string(),
            },
        )];
        let mut view = OpenSessionView::from_loaded(&canonical_read(
            &session,
            0,
            80,
            81,
            prefix_items.clone(),
        ));
        let suffix = canonical_read(&session, 81, 80, 82, suffix_items);

        assert!(!view.merge_contiguous(&suffix));
        assert!(view.refresh_metadata_preserving_loaded_history(&suffix));
        assert_eq!(view.read.turns.offset, 0);
        assert_eq!(view.read.turns.items[0].id, prefix_items[0].id);
        assert_eq!(view.read.turns.total, 82);
        assert!(view.read.turns.has_more);
        assert!(
            view.stored_detail()
                .transcript_rows
                .iter()
                .any(|row| row.body.contains("retained prefix"))
        );
        assert!(
            !view
                .stored_detail()
                .transcript_rows
                .iter()
                .any(|row| row.body.contains("noncontiguous suffix"))
        );
    }

    #[test]
    fn stale_running_prepend_cannot_regress_newer_terminal_session_metadata() {
        let completed = session();
        let turn_id = TurnId::new();
        let items = vec![turn_item(
            completed.id,
            turn_id,
            1,
            TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Completed,
            },
        )];
        let mut current = canonical_read(&completed, 0, 1, 1, items.clone());
        current.latest_turn_id = Some(turn_id);
        let mut stale_running_session = completed.clone();
        stale_running_session.status = SessionStatus::Running;
        stale_running_session.updated_at_ms = completed.updated_at_ms.saturating_sub(1);
        stale_running_session.completed_at_ms = None;
        let mut stale = canonical_read(&stale_running_session, 0, 1, 1, items);
        stale.active_turn_id = Some(turn_id);
        let mut view = OpenSessionView::from_loaded(&current);

        assert!(view.merge_contiguous(&stale));
        assert_eq!(view.read.session.status, SessionStatus::Completed);
        assert_eq!(view.read.active_turn_id, None);
        assert_eq!(view.read.turns.session.status, SessionStatus::Completed);
    }
}
