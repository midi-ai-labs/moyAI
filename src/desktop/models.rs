use crate::session::SessionId;

#[derive(Debug, Clone)]
pub struct DesktopSessionRow {
    pub session_id: SessionId,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct DesktopSessionDetail {
    pub session_id: SessionId,
    pub transcript_text: String,
    pub tool_status_text: String,
    pub progress_text: String,
    pub run_status_text: String,
    pub confirmation_text: String,
    pub confirmation_visible: bool,
}

#[derive(Debug, Clone)]
pub struct DesktopSnapshot {
    pub workspace_path: String,
    pub provider_label: String,
    pub model_label: String,
    pub session_rows: Vec<DesktopSessionRow>,
    pub session_details: Vec<DesktopSessionDetail>,
    pub selected_session_index: usize,
}

impl DesktopSnapshot {
    pub fn selected_session_id(&self) -> Option<SessionId> {
        self.session_rows
            .get(self.selected_session_index)
            .map(|row| row.session_id)
    }

    pub fn detail_for(&self, session_id: SessionId) -> Option<&DesktopSessionDetail> {
        self.session_details
            .iter()
            .find(|detail| detail.session_id == session_id)
    }
}
