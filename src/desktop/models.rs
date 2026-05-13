use crate::session::{ProjectId, SessionId};

#[derive(Debug, Clone)]
pub struct DesktopTranscriptRow {
    pub kind: String,
    pub step: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct DesktopArtifactRow {
    pub label: String,
    pub path: String,
    pub kind: String,
    pub action: String,
}

#[derive(Debug, Clone)]
pub struct DesktopFileChangeRow {
    pub label: String,
    pub path: String,
    pub action: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct DesktopCommandRow {
    pub name: String,
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct DesktopProjectRow {
    pub project_id: ProjectId,
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct DesktopSessionRow {
    pub session_id: SessionId,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct DesktopSessionDetail {
    pub session_id: SessionId,
    pub transcript_text: String,
    pub transcript_rows: Vec<DesktopTranscriptRow>,
    pub tool_status_text: String,
    pub progress_text: String,
    pub run_status_text: String,
    pub confirmation_text: String,
    pub confirmation_visible: bool,
    pub artifacts: Vec<DesktopArtifactRow>,
    pub file_changes: Vec<DesktopFileChangeRow>,
    pub file_change_summary_text: String,
    pub artifact_preview_text: String,
}

#[derive(Debug, Clone)]
pub struct DesktopSnapshot {
    pub workspace_path: String,
    pub provider_label: String,
    pub model_label: String,
    pub command_rows: Vec<DesktopCommandRow>,
    pub project_rows: Vec<DesktopProjectRow>,
    pub selected_project_index: usize,
    pub session_rows: Vec<DesktopSessionRow>,
    pub session_details: Vec<DesktopSessionDetail>,
    pub selected_session_index: usize,
}

impl DesktopSnapshot {
    pub fn selected_project_id(&self) -> Option<ProjectId> {
        self.project_rows
            .get(self.selected_project_index)
            .map(|row| row.project_id)
    }

    pub fn selected_project_path(&self) -> Option<&str> {
        self.project_rows
            .get(self.selected_project_index)
            .map(|row| row.path.as_str())
    }

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
