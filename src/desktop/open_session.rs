use crate::protocol::TurnItem;
use crate::session::{SessionRecord, SessionStateSnapshot, TodoItem, Transcript};
use crate::tui::state::AppState;

use super::models::DesktopSessionDetail;
use super::query::{build_session_detail, build_session_detail_from_app_state_with_session};

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
    ) -> Self {
        let stored_detail = build_session_detail(
            session,
            state,
            todos,
            transcript.clone(),
            turn_items.to_vec(),
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
        if detail.artifacts.is_empty() {
            let fallback = fallback_snapshot_detail.unwrap_or(&self.stored_detail);
            detail.artifacts = fallback.artifacts.clone();
            detail.file_changes = fallback.file_changes.clone();
            detail.file_change_summary_text = fallback.file_change_summary_text.clone();
            detail.artifact_preview_text = fallback.artifact_preview_text.clone();
        }
        detail
    }

    pub fn stored_detail(&self) -> &DesktopSessionDetail {
        &self.stored_detail
    }
}
