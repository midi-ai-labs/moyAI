use crate::protocol::TurnItem;
use crate::session::{SessionRecord, SessionStateSnapshot, TodoItem, Transcript};
use crate::tui::state::{AppState, RunStatus};

use super::models::{DesktopSessionDetail, DesktopTranscriptRowKind};
use super::query::{
    build_session_detail_from_app_state_with_session, build_session_detail_with_page,
};

#[derive(Debug, Clone)]
pub struct OpenSessionView {
    session: SessionRecord,
    stored_detail: DesktopSessionDetail,
}

impl OpenSessionView {
    pub fn from_loaded(
        session: &SessionRecord,
        transcript: &Transcript,
        turn_items: &[TurnItem],
        state: SessionStateSnapshot,
        todos: Vec<TodoItem>,
        turn_page_offset: usize,
        turn_page_limit: usize,
        turn_page_total: usize,
        turn_page_has_more: bool,
    ) -> Self {
        let stored_detail = build_session_detail_with_page(
            session,
            state,
            todos,
            transcript.clone(),
            turn_items.to_vec(),
            turn_page_offset,
            turn_page_limit,
            turn_page_total,
            turn_page_has_more,
            None,
        );
        Self {
            session: session.clone(),
            stored_detail,
        }
    }

    pub fn session(&self) -> &SessionRecord {
        &self.session
    }

    pub fn session_id(&self) -> crate::session::SessionId {
        self.session.id
    }

    pub fn live_detail(
        &self,
        app_state: &AppState,
        fallback_snapshot_detail: Option<&DesktopSessionDetail>,
    ) -> DesktopSessionDetail {
        let mut detail =
            build_session_detail_from_app_state_with_session(app_state, Some(&self.session));
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
        let stored_has_turn_scoped_file_changes = self
            .stored_detail
            .transcript_rows
            .iter()
            .any(|row| row.row_kind == DesktopTranscriptRowKind::FileChanges);
        let live_missing_turn_scoped_file_changes = !detail
            .transcript_rows
            .iter()
            .any(|row| row.row_kind == DesktopTranscriptRowKind::FileChanges);
        if stored_has_turn_scoped_file_changes
            && live_missing_turn_scoped_file_changes
            && !matches!(app_state.run_status, RunStatus::Running)
        {
            detail.transcript_rows = self.stored_detail.transcript_rows.clone();
            detail.transcript_text = self.stored_detail.transcript_text.clone();
        }
        detail
    }

    pub fn stored_detail(&self) -> &DesktopSessionDetail {
        &self.stored_detail
    }
}
