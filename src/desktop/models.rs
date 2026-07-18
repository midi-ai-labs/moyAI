use crate::protocol::TurnId;
use crate::session::{
    ChangeKind, LoadedSessionStatus, LoadedSessionSummary, ProjectId, SessionId, SessionStatus,
    ToolCallId,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTranscriptRowKind {
    EmptyPlaceholder,
    User,
    Assistant,
    ReasoningSummary,
    Editing,
    Tool,
    Diff,
    System,
    Error,
    WorkSummaryRunning,
    WorkSummaryIncomplete,
    WorkSummaryCompleted,
    WorkSummaryFailed,
    WorkSummaryCancelled,
    FileChanges,
}

fn default_desktop_transcript_row_kind() -> DesktopTranscriptRowKind {
    DesktopTranscriptRowKind::System
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopTranscriptRow {
    #[serde(default = "default_desktop_transcript_row_kind")]
    pub row_kind: DesktopTranscriptRowKind,
    pub step: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub file_changes: Vec<DesktopFileChangeRow>,
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
    pub kind: ChangeKind,
    pub action: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_call_ids: Vec<ToolCallId>,
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
    pub status: SessionStatus,
    pub loaded_status: LoadedSessionStatus,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_sequence_no: Option<i64>,
    #[serde(default)]
    pub pending_permission_requests: u32,
    #[serde(default)]
    pub pending_user_input_requests: u32,
    pub short_id: String,
    pub label: String,
}

impl DesktopSessionRow {
    pub(crate) fn from_loaded_summary(summary: &LoadedSessionSummary) -> Self {
        let mut row = Self::from_parts_with_loaded(
            summary.session.id,
            &summary.session.title,
            summary.session.status,
            summary.loaded_status,
            summary.active_turn_id,
            summary.active_turn_sequence_no,
            summary.pending_permission_requests,
            summary.pending_user_input_requests,
        );
        row.archived = summary.archived;
        row
    }

    pub(crate) fn set_title_preserving_status(&mut self, title: &str) {
        let status = self.status;
        self.apply_parts(title, status);
    }

    pub(crate) fn set_status(&mut self, status: SessionStatus) {
        let title = self.title.clone();
        match status {
            SessionStatus::Running => {
                self.loaded_status = LoadedSessionStatus::Active;
            }
            SessionStatus::Idle | SessionStatus::Completed | SessionStatus::Cancelled => {
                self.loaded_status = LoadedSessionStatus::Idle;
                self.clear_active_projection();
            }
            SessionStatus::Failed => {
                self.loaded_status = LoadedSessionStatus::SystemError;
                self.clear_active_projection();
            }
        }
        self.apply_parts(&title, status);
    }

    fn clear_active_projection(&mut self) {
        self.active_turn_id = None;
        self.active_turn_sequence_no = None;
        self.pending_permission_requests = 0;
        self.pending_user_input_requests = 0;
    }

    #[cfg(test)]
    pub(crate) fn from_parts(session_id: SessionId, title: &str, status: SessionStatus) -> Self {
        Self::from_parts_with_loaded(
            session_id,
            title,
            status,
            crate::session::LoadedSessionStatus::NotLoaded,
            None,
            None,
            0,
            0,
        )
    }

    pub(crate) fn from_parts_with_loaded(
        session_id: SessionId,
        title: &str,
        status: SessionStatus,
        loaded_status: LoadedSessionStatus,
        active_turn_id: Option<TurnId>,
        active_turn_sequence_no: Option<i64>,
        pending_permission_requests: u32,
        pending_user_input_requests: u32,
    ) -> Self {
        let mut row = Self {
            session_id,
            title: String::new(),
            status,
            loaded_status,
            archived: false,
            active_turn_id,
            active_turn_sequence_no,
            pending_permission_requests,
            pending_user_input_requests,
            short_id: short_session_id(session_id),
            label: String::new(),
        };
        row.apply_parts(title, status);
        row
    }

    fn apply_parts(&mut self, title: &str, status: SessionStatus) {
        self.title = truncate_text(title.trim(), 24);
        self.status = status;
        self.short_id = short_session_id(self.session_id);
        self.label = format_session_row_parts(title, status, self.session_id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSessionDetail {
    pub session_id: SessionId,
    pub thread_empty: bool,
    pub transcript_text: String,
    pub transcript_rows: Vec<DesktopTranscriptRow>,
    #[serde(default)]
    pub turn_page_offset: usize,
    #[serde(default)]
    pub turn_page_limit: usize,
    #[serde(default)]
    pub turn_page_total: usize,
    #[serde(default)]
    pub turn_page_has_more: bool,
    pub tool_status_text: String,
    pub progress_text: String,
    pub run_status_text: String,
    pub artifacts: Vec<DesktopArtifactRow>,
    pub file_changes: Vec<DesktopFileChangeRow>,
    pub file_change_summary_text: String,
    pub artifact_preview_available: bool,
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

    pub fn replace_detail(&mut self, detail: DesktopSessionDetail) {
        if let Some(existing) = self
            .session_details
            .iter_mut()
            .find(|existing| existing.session_id == detail.session_id)
        {
            *existing = detail;
        } else {
            self.session_details.push(detail);
        }
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

pub(crate) fn format_session_status(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "待機中",
        SessionStatus::Running => "実行中",
        SessionStatus::Completed => "完了",
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
        let turn_id = TurnId::new();
        let mut row = DesktopSessionRow::from_parts_with_loaded(
            session_id,
            "docx/xlsx要約",
            SessionStatus::Running,
            LoadedSessionStatus::Active,
            Some(turn_id),
            Some(3),
            1,
            2,
        );

        row.set_status(SessionStatus::Completed);

        assert_eq!(
            row.label,
            format_session_row_parts("docx/xlsx要約", SessionStatus::Completed, session_id)
        );
        assert!(!row.label.contains("[実行中]"));
        assert_eq!(row.loaded_status, LoadedSessionStatus::Idle);
        assert_eq!(row.active_turn_id, None);
        assert_eq!(row.active_turn_sequence_no, None);
        assert_eq!(row.pending_permission_requests, 0);
        assert_eq!(row.pending_user_input_requests, 0);
    }

    #[test]
    fn session_row_run_status_projects_loaded_capabilities_immediately() {
        let session_id = SessionId::new();
        let mut row = DesktopSessionRow::from_parts(session_id, "new run", SessionStatus::Idle);

        row.set_status(SessionStatus::Running);
        assert_eq!(row.loaded_status, LoadedSessionStatus::Active);

        row.set_status(SessionStatus::Failed);
        assert_eq!(row.loaded_status, LoadedSessionStatus::SystemError);
        assert_eq!(row.active_turn_id, None);
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

    #[test]
    fn desktop_session_row_loaded_active_projection() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let row = DesktopSessionRow::from_parts_with_loaded(
            session_id,
            "running thread",
            SessionStatus::Running,
            LoadedSessionStatus::Active,
            Some(turn_id),
            Some(7),
            1,
            2,
        );

        assert_eq!(row.status, SessionStatus::Running);
        assert_eq!(row.loaded_status, LoadedSessionStatus::Active);
        assert_eq!(row.active_turn_id, Some(turn_id));
        assert_eq!(row.active_turn_sequence_no, Some(7));
        assert_eq!(row.pending_permission_requests, 1);
        assert_eq!(row.pending_user_input_requests, 2);
        assert_eq!(
            row.label,
            format_session_row_parts("running thread", SessionStatus::Running, session_id)
        );
    }
}
