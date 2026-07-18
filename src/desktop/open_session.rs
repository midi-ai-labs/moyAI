use crate::session::{CanonicalSessionRead, SessionRecord};
use crate::tui::state::AppState;

use super::models::DesktopSessionDetail;
use super::query::{build_session_detail, build_session_detail_from_app_state_with_session};

#[derive(Debug, Clone)]
pub struct OpenSessionView {
    session: SessionRecord,
    stored_detail: DesktopSessionDetail,
}

impl OpenSessionView {
    pub fn from_loaded(read: &CanonicalSessionRead) -> Self {
        let stored_detail = build_session_detail(read, None);
        Self {
            session: read.session.clone(),
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
        detail.artifact_preview_available = !detail.artifacts.is_empty();
        detail
    }

    pub fn stored_detail(&self) -> &DesktopSessionDetail {
        &self.stored_detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::models::{
        DesktopArtifactRow, DesktopTranscriptRow, DesktopTranscriptRowKind,
    };
    use crate::session::{ProjectId, SessionId, SessionStatus};
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
            session,
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
}
