use crate::session::{ProjectId, SessionId, SessionRecord, SessionStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopTranscriptRow {
    pub kind: String,
    pub step: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopArtifactRow {
    pub label: String,
    pub path: String,
    pub kind: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopFileChangeRow {
    pub label: String,
    pub path: String,
    pub action: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopCommandRow {
    pub name: String,
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopProjectRow {
    pub project_id: ProjectId,
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSessionRow {
    pub session_id: SessionId,
    pub title: String,
    pub status: String,
    pub short_id: String,
    pub label: String,
}

impl DesktopSessionRow {
    pub(crate) fn from_session(session: &SessionRecord) -> Self {
        Self::from_parts(session.id, &session.title, session.status)
    }

    pub(crate) fn set_title_preserving_status(&mut self, title: &str) {
        let status = session_status_from_key(&self.status).unwrap_or(SessionStatus::Running);
        self.apply_parts(title, status);
    }

    pub(crate) fn set_status(&mut self, status: SessionStatus) {
        let title = self.title.clone();
        self.apply_parts(&title, status);
    }

    pub(crate) fn from_parts(session_id: SessionId, title: &str, status: SessionStatus) -> Self {
        let mut row = Self {
            session_id,
            title: String::new(),
            status: String::new(),
            short_id: short_session_id(session_id),
            label: String::new(),
        };
        row.apply_parts(title, status);
        row
    }

    fn apply_parts(&mut self, title: &str, status: SessionStatus) {
        self.title = truncate_text(title.trim(), 24);
        self.status = session_status_key(status).to_string();
        self.short_id = short_session_id(self.session_id);
        self.label = format_session_row_parts(title, status, self.session_id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSnapshot {
    pub workspace_path: String,
    pub provider_label: String,
    pub model_label: String,
    pub command_rows: Vec<DesktopCommandRow>,
    pub project_rows: Vec<DesktopProjectRow>,
    pub selected_project_index: usize,
    pub session_rows: Vec<DesktopSessionRow>,
    pub chat_session_rows: Vec<DesktopSessionRow>,
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

pub(crate) fn format_session_row_parts(
    title: &str,
    status: SessionStatus,
    session_id: SessionId,
) -> String {
    format!(
        "{} [{}] {}",
        truncate_text(title.trim(), 24),
        format_session_status(status),
        short_session_id(session_id)
    )
}

fn session_status_from_key(key: &str) -> Option<SessionStatus> {
    match key {
        "idle" => Some(SessionStatus::Idle),
        "running" => Some(SessionStatus::Running),
        "completed" => Some(SessionStatus::Completed),
        "awaiting_user" => Some(SessionStatus::AwaitingUser),
        "cancelled" => Some(SessionStatus::Cancelled),
        "failed" => Some(SessionStatus::Failed),
        _ => None,
    }
}

fn session_status_key(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Completed => "completed",
        SessionStatus::AwaitingUser => "awaiting_user",
        SessionStatus::Cancelled => "cancelled",
        SessionStatus::Failed => "failed",
    }
}

pub(crate) fn format_session_status(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "待機中",
        SessionStatus::Running => "実行中",
        SessionStatus::Completed => "完了",
        SessionStatus::AwaitingUser => "確認待ち",
        SessionStatus::Cancelled => "停止済み",
        SessionStatus::Failed => "失敗",
    }
}

fn short_session_id(session_id: SessionId) -> String {
    session_id.to_string().chars().take(8).collect()
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let shortened = value.chars().take(keep).collect::<String>();
    format!("{shortened}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_row_terminal_status_replaces_running_label() {
        let session_id = SessionId::new();
        let mut row =
            DesktopSessionRow::from_parts(session_id, "docx/xlsx要約", SessionStatus::Running);

        row.set_status(SessionStatus::Completed);

        assert_eq!(
            row.label,
            format_session_row_parts("docx/xlsx要約", SessionStatus::Completed, session_id)
        );
        assert!(!row.label.contains("[実行中]"));
    }

    #[test]
    fn session_row_title_update_preserves_status_and_short_id() {
        let session_id = SessionId::new();
        let mut row =
            DesktopSessionRow::from_parts(session_id, "新規チャット", SessionStatus::Running);

        row.set_title_preserving_status("ワークスペースの資材");

        assert!(row.label.contains("ワークスペースの資材"));
        assert!(row.label.contains("[実行中]"));
        assert!(row.label.ends_with(&short_session_id(session_id)));
    }
}
