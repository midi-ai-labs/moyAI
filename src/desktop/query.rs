use crate::app::App;
use crate::desktop::args::{DesktopArgs, quick_chat_workspace_directory};
use crate::desktop::models::{
    DesktopCommandRow, DesktopFileChangeRow, DesktopProjectRow, DesktopSessionDetail,
    DesktopSessionRow, DesktopSnapshot, DesktopTranscriptRow, DesktopTranscriptRowKind,
    format_session_status,
};
use crate::error::AppRunError;
use crate::harness::ReplayReport;
use crate::session::{
    CanonicalSessionRead, LoadedSessionSummary, ProjectId, ProjectRecord, SessionId, SessionRecord,
    SessionStatus, ToolCallStatus,
};
use crate::tui::state::{AppState, RunProgressPhase, RunStatus, TranscriptKind};

use super::artifact_projection::{
    artifact_rows_from_file_changes, file_change_rows_from_turn_items_with_root,
    format_file_change_summary,
};
pub use super::artifact_projection::{file_change_rows_from_turn_items, format_artifact_preview};

pub const DESKTOP_TURN_PAGE_LIMIT: usize = 80;
pub(crate) const DESKTOP_HISTORY_PROJECTION_LIMIT: usize = 1;
const DESKTOP_COMMAND_ROW_LIMIT: usize = 64;
const DESKTOP_COMMAND_SCAN_LIMIT: usize = 512;

pub struct LoadedSessionDetail {
    pub read: CanonicalSessionRead,
}

pub async fn load_snapshot(app: &App, args: &DesktopArgs) -> Result<DesktopSnapshot, AppRunError> {
    load_snapshot_for_selection(app, args.session_id).await
}

pub async fn load_snapshot_for_selection(
    app: &App,
    selected_session_id: Option<SessionId>,
) -> Result<DesktopSnapshot, AppRunError> {
    let mut loaded = app
        .session_service
        .loaded_sessions(app.workspace.project_id, 20, false)
        .await?;
    if let Some(session_id) = selected_session_id
        && !loaded
            .sessions
            .iter()
            .any(|summary| summary.session.id == session_id)
    {
        let session = app.session_service.get_session(session_id).await?;
        if session.project_id != app.workspace.project_id {
            return Err(AppRunError::Message(format!(
                "session {} was not found in this workspace",
                session_id
            )));
        }
        let summary = app.session_service.loaded_session_summary(session).await?;
        loaded.sessions.insert(0, summary);
    }
    let sessions = loaded
        .sessions
        .iter()
        .map(|summary| summary.session.clone())
        .collect::<Vec<_>>();
    let selected_session_index = select_session_index(
        &sessions,
        selected_session_id,
        Some(app.workspace.project_id),
        false,
    )?;
    build_snapshot(app, loaded.sessions, selected_session_index).await
}

pub async fn load_snapshot_continue_last(app: &App) -> Result<DesktopSnapshot, AppRunError> {
    let loaded = app
        .session_service
        .loaded_sessions(app.workspace.project_id, 20, false)
        .await?;
    let sessions = loaded
        .sessions
        .iter()
        .map(|summary| summary.session.clone())
        .collect::<Vec<_>>();
    let selected_session_index =
        select_session_index(&sessions, None, Some(app.workspace.project_id), true)?;
    build_snapshot(app, loaded.sessions, selected_session_index).await
}

pub async fn load_snapshot_for_session_search(
    app: &App,
    query: &str,
    include_archived: bool,
    selected_session_id: Option<SessionId>,
) -> Result<DesktopSnapshot, AppRunError> {
    let query = query.trim();
    let loaded = app
        .session_service
        .search_loaded_sessions(app.workspace.project_id, query, 50, include_archived)
        .await?;
    let sessions = loaded
        .sessions
        .iter()
        .map(|summary| summary.session.clone())
        .collect::<Vec<_>>();
    let selected_session_index = select_session_index(
        &sessions,
        selected_session_id,
        Some(app.workspace.project_id),
        false,
    )
    .unwrap_or(0);
    build_snapshot(app, loaded.sessions, selected_session_index).await
}

pub async fn load_session_detail(
    app: &App,
    session_id: SessionId,
) -> Result<LoadedSessionDetail, AppRunError> {
    load_latest_session_detail(app, session_id).await
}

pub async fn load_latest_session_detail(
    app: &App,
    session_id: SessionId,
) -> Result<LoadedSessionDetail, AppRunError> {
    let snapshot = app
        .session_service
        .canonical_latest_session_snapshot(
            session_id,
            DESKTOP_HISTORY_PROJECTION_LIMIT,
            DESKTOP_TURN_PAGE_LIMIT,
        )
        .await?;
    Ok(LoadedSessionDetail {
        read: snapshot.read,
    })
}

async fn build_snapshot(
    app: &App,
    sessions: Vec<LoadedSessionSummary>,
    selected_session_index: usize,
) -> Result<DesktopSnapshot, AppRunError> {
    let mut session_rows = Vec::with_capacity(sessions.len());
    let projects = app.session_service.list_projects(30).await?;
    let hidden_roots =
        internal_desktop_project_roots(app.session_service.store.paths().data_dir.as_path());
    let (project_rows, selected_project_index) = build_project_rows(
        &projects,
        app.workspace.project_id,
        &app.workspace.root,
        &hidden_roots,
    );
    let chat_session_rows = build_quick_chat_session_rows(app, &projects).await?;
    for summary in &sessions {
        session_rows.push(DesktopSessionRow::from_loaded_summary(summary));
    }
    Ok(DesktopSnapshot {
        workspace_path: app.workspace.root.to_string(),
        provider_label: app.config.model.base_url.clone(),
        model_label: app.config.model.model.clone(),
        command_rows: load_command_rows(&app.workspace.root),
        project_rows,
        selected_project_index,
        session_rows,
        chat_session_rows,
        session_details: Vec::new(),
        selected_session_index,
    })
}

async fn build_quick_chat_session_rows(
    app: &App,
    projects: &[ProjectRecord],
) -> Result<Vec<DesktopSessionRow>, AppRunError> {
    let Some(root) = quick_chat_workspace_directory() else {
        return Ok(Vec::new());
    };
    let project_id = projects
        .iter()
        .find(|project| project.root_path.as_path() == root.as_path())
        .map(|project| project.id);
    let project_id = match project_id {
        Some(project_id) => project_id,
        None => {
            let Some(project_id) = app
                .session_service
                .list_projects(200)
                .await?
                .into_iter()
                .find(|project| project.root_path.as_path() == root.as_path())
                .map(|project| project.id)
            else {
                return Ok(Vec::new());
            };
            project_id
        }
    };
    let sessions = app
        .session_service
        .loaded_sessions(project_id, 20, false)
        .await?;
    Ok(sessions
        .sessions
        .iter()
        .map(DesktopSessionRow::from_loaded_summary)
        .collect())
}

fn build_project_rows(
    projects: &[ProjectRecord],
    current_project_id: ProjectId,
    current_path: &camino::Utf8Path,
    hidden_roots: &[camino::Utf8PathBuf],
) -> (Vec<DesktopProjectRow>, usize) {
    let mut rows = projects
        .iter()
        .filter(|project| {
            !hidden_roots
                .iter()
                .any(|root| root.as_path() == project.root_path.as_path())
        })
        .map(|project| DesktopProjectRow {
            project_id: project.id,
            label: format_project_row(project),
            path: project.root_path.to_string(),
        })
        .collect::<Vec<_>>();
    if hidden_roots
        .iter()
        .any(|root| root.as_path() == current_path)
    {
        let selected = rows.len();
        return (rows, selected);
    }
    if !rows
        .iter()
        .any(|project| project.project_id == current_project_id)
    {
        rows.insert(
            0,
            DesktopProjectRow {
                project_id: current_project_id,
                label: project_folder_label(current_path),
                path: current_path.to_string(),
            },
        );
    }
    let selected = rows
        .iter()
        .position(|project| project.project_id == current_project_id)
        .unwrap_or(rows.len());
    (rows, selected)
}

fn internal_desktop_project_roots(data_dir: &camino::Utf8Path) -> Vec<camino::Utf8PathBuf> {
    [
        "quick-chat-workspace",
        "desktop-workspace",
        "desktop-workspace-after-delete",
        "desktop-workspace-after-delete-2",
    ]
    .into_iter()
    .map(|name| data_dir.join(name))
    .collect()
}

fn format_project_row(project: &ProjectRecord) -> String {
    truncate_text(&project_folder_label(&project.root_path), 34)
}

fn project_folder_label(path: &camino::Utf8Path) -> String {
    path.file_name()
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

pub fn select_session_index(
    sessions: &[SessionRecord],
    session_id: Option<SessionId>,
    preferred_project_id: Option<ProjectId>,
    continue_last: bool,
) -> Result<usize, AppRunError> {
    if sessions.is_empty() {
        return Ok(0);
    }
    if continue_last {
        return Ok(0);
    }
    if let Some(session_id) = session_id {
        return sessions
            .iter()
            .position(|session| session.id == session_id)
            .ok_or_else(|| {
                AppRunError::Message(format!(
                    "session {} was not found in this workspace",
                    session_id
                ))
            });
    }
    if let Some(project_id) = preferred_project_id
        && let Some(index) = sessions
            .iter()
            .position(|session| session.project_id == project_id)
    {
        return Ok(index);
    }
    Ok(0)
}

pub fn build_session_detail(
    read: &CanonicalSessionRead,
    replay_report: Option<ReplayReport>,
) -> DesktopSessionDetail {
    let session = &read.session;
    let turn_items = &read.turns.items;
    let mut ui_state = AppState::default();
    ui_state.load_turn_items_with_active_turn(session, turn_items, read.active_turn_id);
    let file_changes =
        file_change_rows_from_turn_items_with_root(turn_items, Some(session.cwd.as_path()));
    let mut detail = build_session_detail_from_app_state(&ui_state);
    detail.turn_page_offset = read.turns.offset;
    detail.turn_page_limit = if read.turns.limit == 0 {
        turn_items.len()
    } else {
        read.turns.limit
    };
    detail.turn_page_total = if read.turns.total == 0 {
        turn_items.len()
    } else {
        read.turns.total
    };
    detail.turn_page_has_more = read.turns.has_more;
    detail.session_id = session.id;
    detail.transcript_rows = transcript_rows_from_turn_items_with_context_and_elapsed(
        session,
        turn_items,
        &read.turn_elapsed_ms,
    );
    detail.thread_empty = transcript_rows_are_empty_placeholder(&detail.transcript_rows);
    detail.artifacts = artifact_rows_from_file_changes(&file_changes);
    detail.file_change_summary_text = format_file_change_summary(&file_changes);
    detail.artifact_preview_text = format_artifact_preview(detail.artifacts.first(), &file_changes);
    detail.artifact_preview_available = !detail.artifacts.is_empty();
    detail.file_changes = file_changes;
    if let Some(report) = replay_report {
        append_replay_summary(&mut detail.tool_status_text, &report);
    }
    detail
}

#[derive(Default)]
struct TurnTranscriptGroup {
    turn_id: Option<crate::protocol::TurnId>,
    user_body: String,
    assistant_bodies: Vec<String>,
    tool_rows: Vec<String>,
    file_change_items: Vec<crate::protocol::TurnItem>,
    system_rows: Vec<DesktopTranscriptRow>,
    agent_rows: Vec<DesktopTranscriptRow>,
    terminal_outcome: Option<crate::protocol::TurnTerminalOutcome>,
}

impl TurnTranscriptGroup {
    fn has_content(&self) -> bool {
        !self.user_body.trim().is_empty()
            || !self.assistant_bodies.is_empty()
            || !self.tool_rows.is_empty()
            || !self.file_change_items.is_empty()
            || !self.system_rows.is_empty()
            || !self.agent_rows.is_empty()
            || self.terminal_outcome.is_some()
    }
}

#[cfg(test)]
pub(super) fn transcript_rows_from_turn_items_with_context(
    session: &SessionRecord,
    turn_items: &[crate::protocol::TurnItem],
) -> Vec<DesktopTranscriptRow> {
    transcript_rows_from_turn_items_with_context_and_elapsed(
        session,
        turn_items,
        &std::collections::HashMap::new(),
    )
}

pub(super) fn transcript_rows_from_turn_items_with_context_and_elapsed(
    session: &SessionRecord,
    turn_items: &[crate::protocol::TurnItem],
    turn_elapsed_ms: &std::collections::HashMap<crate::protocol::TurnId, u64>,
) -> Vec<DesktopTranscriptRow> {
    let mut rows = Vec::new();
    let mut current = TurnTranscriptGroup::default();
    let ordered = ordered_turn_items_for_projection(turn_items);
    let mut immediately_preceding_agent_communication: Option<String> = None;

    for item in ordered {
        if current
            .turn_id
            .is_some_and(|turn_id| turn_id != item.turn_id)
        {
            let elapsed_ms = current
                .turn_id
                .and_then(|turn_id| turn_elapsed_ms.get(&turn_id).copied());
            flush_turn_transcript_group(&mut rows, session, &mut current, elapsed_ms, None, None);
        }
        current.turn_id.get_or_insert(item.turn_id);
        let preceding_agent_communication = immediately_preceding_agent_communication.take();
        match &item.payload {
            crate::protocol::TurnItemPayload::UserMessage { text } => {
                let elapsed_ms = current
                    .turn_id
                    .and_then(|turn_id| turn_elapsed_ms.get(&turn_id).copied());
                flush_turn_transcript_group(
                    &mut rows,
                    session,
                    &mut current,
                    elapsed_ms,
                    None,
                    Some(item.id),
                );
                current.turn_id = Some(item.turn_id);
                current.user_body = text.clone();
            }
            crate::protocol::TurnItemPayload::SteerMessage { text } => {
                let elapsed_ms = current
                    .turn_id
                    .and_then(|turn_id| turn_elapsed_ms.get(&turn_id).copied());
                flush_turn_transcript_group(
                    &mut rows,
                    session,
                    &mut current,
                    elapsed_ms,
                    None,
                    Some(item.id),
                );
                current.turn_id = Some(item.turn_id);
                current.user_body = text.clone();
            }
            crate::protocol::TurnItemPayload::AgentMessage { text } => {
                current.assistant_bodies.push(text.clone());
            }
            crate::protocol::TurnItemPayload::InterAgentCommunication { communication } => {
                if communication.recipient == "/root" {
                    current.agent_rows.push(desktop_transcript_row(
                        DesktopTranscriptRowKind::SubAgentUpdated,
                        String::new(),
                        communication.author.clone(),
                        String::new(),
                        Vec::new(),
                    ));
                    immediately_preceding_agent_communication = Some(communication.author.clone());
                } else {
                    current.system_rows.push(desktop_transcript_row(
                        DesktopTranscriptRowKind::System,
                        String::new(),
                        "Agent間の追加指示".to_string(),
                        communication.content.clone(),
                        Vec::new(),
                    ));
                }
            }
            crate::protocol::TurnItemPayload::ToolStatus {
                title,
                status,
                summary,
                ..
            } => {
                current.tool_rows.push(format_tool_history_row(
                    *status,
                    title.trim(),
                    summary.trim(),
                ));
            }
            crate::protocol::TurnItemPayload::FileChange { .. } => {
                current.file_change_items.push((*item).clone());
            }
            crate::protocol::TurnItemPayload::ContextCompaction { summary } => {
                current.system_rows.push(desktop_transcript_row(
                    DesktopTranscriptRowKind::System,
                    String::new(),
                    "システム - Context Compaction".to_string(),
                    format!("圧縮しました\n\n{}", summary.trim()),
                    Vec::new(),
                ));
            }
            crate::protocol::TurnItemPayload::ApprovalRequest { summary, .. } => {
                current.system_rows.push(desktop_transcript_row(
                    DesktopTranscriptRowKind::System,
                    String::new(),
                    "確認".to_string(),
                    summary.clone(),
                    Vec::new(),
                ));
            }
            crate::protocol::TurnItemPayload::Warning { message } => {
                current.system_rows.push(desktop_transcript_row(
                    DesktopTranscriptRowKind::System,
                    String::new(),
                    "警告".to_string(),
                    message.clone(),
                    Vec::new(),
                ));
            }
            crate::protocol::TurnItemPayload::Error { message } => {
                current.system_rows.push(desktop_transcript_row(
                    DesktopTranscriptRowKind::Error,
                    String::new(),
                    "エラー".to_string(),
                    message.clone(),
                    Vec::new(),
                ));
            }
            crate::protocol::TurnItemPayload::Terminal { outcome } => {
                current.terminal_outcome = Some(outcome.clone());
            }
            crate::protocol::TurnItemPayload::SubAgentActivity {
                agent_path,
                activity_kind,
                ..
            } => {
                let row_kind = match activity_kind {
                    crate::protocol::SubAgentActivityKind::Started => {
                        DesktopTranscriptRowKind::SubAgentStarted
                    }
                    crate::protocol::SubAgentActivityKind::Interacted => {
                        DesktopTranscriptRowKind::SubAgentUpdated
                    }
                    crate::protocol::SubAgentActivityKind::Interrupted => {
                        DesktopTranscriptRowKind::SubAgentInterrupted
                    }
                };
                if row_kind == DesktopTranscriptRowKind::SubAgentUpdated
                    && preceding_agent_communication.as_deref() == Some(agent_path.as_str())
                    && let Some(communication_row) = current.agent_rows.last_mut()
                    && communication_row.row_kind == DesktopTranscriptRowKind::SubAgentUpdated
                    && communication_row.title == agent_path.as_str()
                {
                    // send_message persists the durable communication first and
                    // the lifecycle marker immediately afterwards. One compact
                    // activity marker represents that single interaction.
                    communication_row.body.clear();
                    continue;
                }
                current.agent_rows.push(desktop_transcript_row(
                    row_kind,
                    String::new(),
                    agent_path.clone(),
                    String::new(),
                    Vec::new(),
                ));
            }
            crate::protocol::TurnItemPayload::Plan { .. }
            | crate::protocol::TurnItemPayload::WorldState { .. } => {}
        }
    }
    let elapsed_ms = current
        .turn_id
        .and_then(|turn_id| turn_elapsed_ms.get(&turn_id).copied());
    flush_turn_transcript_group(
        &mut rows,
        session,
        &mut current,
        elapsed_ms,
        Some(session.status),
        None,
    );
    if rows.is_empty() {
        rows.push(desktop_transcript_row(
            DesktopTranscriptRowKind::EmptyPlaceholder,
            "00".to_string(),
            "履歴はまだありません".to_string(),
            "依頼を送信すると、ユーザー入力、ツール実行、ファイル変更、最終応答がここに並びます。"
                .to_string(),
            Vec::new(),
        ));
    }
    renumber_rows(rows)
}

fn desktop_transcript_row(
    row_kind: DesktopTranscriptRowKind,
    step: String,
    title: String,
    body: String,
    file_changes: Vec<DesktopFileChangeRow>,
) -> DesktopTranscriptRow {
    DesktopTranscriptRow {
        row_kind,
        stable_history_identity: None,
        step,
        title,
        body,
        file_changes,
    }
}

fn ordered_turn_items_for_projection(
    turn_items: &[crate::protocol::TurnItem],
) -> Vec<&crate::protocol::TurnItem> {
    crate::protocol::turn_items_in_projection_order(turn_items)
}

fn flush_turn_transcript_group(
    rows: &mut Vec<DesktopTranscriptRow>,
    session: &SessionRecord,
    group: &mut TurnTranscriptGroup,
    elapsed_ms: Option<u64>,
    lifecycle_status: Option<SessionStatus>,
    next_user_boundary_id: Option<crate::protocol::TurnItemId>,
) {
    if !group.has_content() {
        group.turn_id = None;
        return;
    }
    if !group.user_body.trim().is_empty() {
        rows.push(desktop_transcript_row(
            DesktopTranscriptRowKind::User,
            String::new(),
            "ユーザー依頼".to_string(),
            group.user_body.trim().to_string(),
            Vec::new(),
        ));
    }
    let has_work_summary = turn_group_has_work_summary(group);
    rows.extend(group.system_rows.drain(..));
    rows.extend(group.agent_rows.drain(..));

    let file_changes = file_change_rows_from_turn_items_with_root(
        &group.file_change_items,
        Some(session.cwd.as_path()),
    );
    if has_work_summary {
        let row_kind = turn_work_summary_kind(group, lifecycle_status);
        let mut row = desktop_transcript_row(
            row_kind,
            String::new(),
            stored_turn_work_summary_title(row_kind, elapsed_ms),
            turn_work_summary_body(group, &file_changes, lifecycle_status),
            Vec::new(),
        );
        row.stable_history_identity = group.turn_id.map(|turn_id| {
            next_user_boundary_id.map_or_else(
                || turn_work_summary_stable_identity(turn_id),
                |boundary_id| turn_work_summary_segment_stable_identity(turn_id, boundary_id),
            )
        });
        rows.push(row);
    }
    for body in primary_assistant_bodies_for_turn_group(group) {
        if body.trim().is_empty() {
            continue;
        }
        rows.push(desktop_transcript_row(
            DesktopTranscriptRowKind::Assistant,
            String::new(),
            "応答".to_string(),
            body.trim().to_string(),
            Vec::new(),
        ));
    }
    if !file_changes.is_empty() {
        rows.push(desktop_transcript_row(
            DesktopTranscriptRowKind::FileChanges,
            String::new(),
            "ファイル変更結果".to_string(),
            file_change_transcript_body(&file_changes),
            file_changes.clone(),
        ));
    }

    group.user_body.clear();
    group.assistant_bodies.clear();
    group.tool_rows.clear();
    group.file_change_items.clear();
    group.terminal_outcome = None;
    group.turn_id = None;
}

pub(super) fn turn_work_summary_stable_identity(turn_id: crate::protocol::TurnId) -> String {
    format!("turn:{turn_id}:work-summary")
}

fn turn_work_summary_segment_stable_identity(
    turn_id: crate::protocol::TurnId,
    next_user_boundary_id: crate::protocol::TurnItemId,
) -> String {
    format!("turn:{turn_id}:before:{next_user_boundary_id}:work-summary")
}

fn turn_work_summary_kind(
    group: &TurnTranscriptGroup,
    lifecycle_status: Option<SessionStatus>,
) -> DesktopTranscriptRowKind {
    match group.terminal_outcome.as_ref() {
        Some(crate::protocol::TurnTerminalOutcome::Failed { .. }) => {
            DesktopTranscriptRowKind::WorkSummaryFailed
        }
        Some(crate::protocol::TurnTerminalOutcome::Interrupted { .. }) => {
            DesktopTranscriptRowKind::WorkSummaryCancelled
        }
        Some(crate::protocol::TurnTerminalOutcome::Completed) => {
            DesktopTranscriptRowKind::WorkSummaryCompleted
        }
        None => match lifecycle_status {
            Some(SessionStatus::Running) => DesktopTranscriptRowKind::WorkSummaryRunning,
            Some(
                SessionStatus::Idle
                | SessionStatus::Completed
                | SessionStatus::Cancelled
                | SessionStatus::Failed,
            )
            | None => DesktopTranscriptRowKind::WorkSummaryIncomplete,
        },
    }
}

fn stored_turn_work_summary_title(
    row_kind: DesktopTranscriptRowKind,
    elapsed_ms: Option<u64>,
) -> String {
    let base = match row_kind {
        DesktopTranscriptRowKind::WorkSummaryRunning => "作業中",
        DesktopTranscriptRowKind::WorkSummaryIncomplete => "状態未確定の作業履歴",
        DesktopTranscriptRowKind::WorkSummaryFailed => "失敗した作業",
        DesktopTranscriptRowKind::WorkSummaryCancelled => "停止した作業",
        DesktopTranscriptRowKind::WorkSummaryCompleted => "作業履歴 / 作業サマリ",
        _ => "作業履歴 / 作業サマリ",
    };
    let Some(elapsed_ms) = elapsed_ms else {
        return base.to_string();
    };
    let elapsed = format_duration(elapsed_ms);
    match row_kind {
        DesktopTranscriptRowKind::WorkSummaryFailed => format!("{elapsed}で失敗しました"),
        DesktopTranscriptRowKind::WorkSummaryCancelled => format!("{elapsed}で停止しました"),
        DesktopTranscriptRowKind::WorkSummaryCompleted => format!("{elapsed}作業しました"),
        _ => base.to_string(),
    }
}

fn turn_group_has_work_summary(group: &TurnTranscriptGroup) -> bool {
    group.has_content()
}

fn primary_assistant_bodies_for_turn_group(group: &TurnTranscriptGroup) -> Vec<String> {
    let bodies = group
        .assistant_bodies
        .iter()
        .map(|body| body.trim())
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>();
    if bodies.len() <= 1 || !turn_group_has_work_summary(group) {
        return bodies.into_iter().map(str::to_string).collect();
    }
    bodies
        .last()
        .map(|body| vec![(*body).to_string()])
        .unwrap_or_default()
}

fn turn_work_summary_body(
    group: &TurnTranscriptGroup,
    file_changes: &[DesktopFileChangeRow],
    lifecycle_status: Option<SessionStatus>,
) -> String {
    let mut sections = Vec::new();
    sections.push(format!(
        "### 作業サマリ\n{}",
        turn_summary_text(group, file_changes, lifecycle_status)
    ));
    if !group.tool_rows.is_empty() || !folded_intermediate_assistant_history_rows(group).is_empty()
    {
        sections.push(format!("### 作業履歴\n{}", turn_work_history_text(group)));
    }
    sections.join("\n\n")
}

fn turn_work_history_text(group: &TurnTranscriptGroup) -> String {
    let mut rows = Vec::new();
    rows.extend(group.tool_rows.iter().take(12).cloned());
    let assistant_previews = folded_intermediate_assistant_history_rows(group);
    if !assistant_previews.is_empty() {
        rows.push("- 中間応答: primary reading path から折りたたみ".to_string());
        rows.extend(assistant_previews);
    }
    rows.join("\n")
}

fn folded_intermediate_assistant_history_rows(group: &TurnTranscriptGroup) -> Vec<String> {
    let bodies = group
        .assistant_bodies
        .iter()
        .map(|body| body.trim())
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>();
    if bodies.len() <= 1 {
        return Vec::new();
    }
    bodies
        .iter()
        .take(bodies.len().saturating_sub(1))
        .take(6)
        .map(|body| format!("  - {}", single_line_preview(body, 160)))
        .collect()
}

fn turn_summary_text(
    group: &TurnTranscriptGroup,
    file_changes: &[DesktopFileChangeRow],
    lifecycle_status: Option<SessionStatus>,
) -> String {
    let status = group
        .terminal_outcome
        .as_ref()
        .map(|outcome| {
            outcome
                .interruption_cause()
                .map(crate::tui::state::interruption_status_message)
                .unwrap_or_else(|| terminal_summary_label(outcome.summary()))
        })
        .unwrap_or_else(|| match lifecycle_status {
            Some(SessionStatus::Running) => "セッションは実行中です。".to_string(),
            Some(
                SessionStatus::Idle
                | SessionStatus::Completed
                | SessionStatus::Cancelled
                | SessionStatus::Failed,
            )
            | None => "この turn の完了状態は未確定です。".to_string(),
        });
    let mut lines = vec![format!("- 結果: {status}")];
    if !file_changes.is_empty() {
        lines.push(format!(
            "- ファイル変更: {}件 ({})",
            file_changes.len(),
            file_changes
                .iter()
                .take(4)
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !group.tool_rows.is_empty() {
        lines.push(format!("- コマンド/ツール: {}件", group.tool_rows.len()));
    }
    lines.join("\n")
}

fn terminal_summary_label(summary: &str) -> String {
    let trimmed = summary.trim();
    match trimmed {
        "completed" | "session completed" => "セッションは完了しました。".to_string(),
        other if other.is_empty() => "作業履歴を記録しました。".to_string(),
        other => other.to_string(),
    }
}

fn format_tool_history_row(
    status: crate::protocol::ToolLifecycleStatus,
    title: &str,
    summary: &str,
) -> String {
    let mut row = format!("- [{}] {}", turn_tool_status_label(status), title);
    if !summary.is_empty() {
        let preview = single_line_preview(summary, 220);
        row.push_str(&format!("\n  出力: {preview}"));
    }
    row
}

fn single_line_preview(value: &str, max_chars: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let keep = max_chars.saturating_sub(1);
    format!("{}…", collapsed.chars().take(keep).collect::<String>())
}

fn file_change_transcript_body(file_changes: &[DesktopFileChangeRow]) -> String {
    file_changes
        .iter()
        .map(|row| {
            let summary = if row.summary.trim().is_empty() {
                String::new()
            } else {
                format!(" - {}", row.summary.trim())
            };
            format!("- [{}] {}{}", row.action, row.path, summary)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn turn_tool_status_label(status: crate::protocol::ToolLifecycleStatus) -> &'static str {
    match status {
        crate::protocol::ToolLifecycleStatus::Pending => "待機",
        crate::protocol::ToolLifecycleStatus::Running => "実行中",
        crate::protocol::ToolLifecycleStatus::Completed => "完了",
        crate::protocol::ToolLifecycleStatus::Declined => "拒否",
        crate::protocol::ToolLifecycleStatus::Cancelled => "キャンセル",
        crate::protocol::ToolLifecycleStatus::Failed => "失敗",
    }
}

pub fn build_session_detail_from_app_state(state: &AppState) -> DesktopSessionDetail {
    build_session_detail_from_app_state_with_session(state, None)
}

pub fn build_session_detail_from_app_state_with_session(
    state: &AppState,
    session: Option<&SessionRecord>,
) -> DesktopSessionDetail {
    let transcript_rows = transcript_rows_with_context(state, session, &[]);
    DesktopSessionDetail {
        session_id: state.current_session_id.unwrap_or_else(SessionId::new),
        thread_empty: transcript_rows_are_empty_placeholder(&transcript_rows),
        transcript_text: format_transcript_text(state),
        transcript_rows,
        turn_page_offset: 0,
        turn_page_limit: 0,
        turn_page_total: 0,
        turn_page_has_more: false,
        tool_status_text: format_tool_status_text(state),
        progress_text: format_progress_text(state),
        run_status_text: format_run_status_text(state),
        artifacts: Vec::new(),
        file_changes: Vec::new(),
        file_change_summary_text: "ファイル変更はまだありません。".to_string(),
        artifact_preview_available: false,
        artifact_preview_text: "アーティファクトは選択されていません。".to_string(),
    }
}

fn transcript_rows_are_empty_placeholder(rows: &[DesktopTranscriptRow]) -> bool {
    matches!(
        rows,
        [DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::EmptyPlaceholder,
            ..
        }]
    )
}

fn load_command_rows(workspace_root: &camino::Utf8Path) -> Vec<DesktopCommandRow> {
    let command_dir = workspace_root.join(".moyai").join("commands");
    let Ok(entries) = std::fs::read_dir(command_dir.as_std_path()) else {
        return Vec::new();
    };
    let mut rows = entries
        .take(DESKTOP_COMMAND_SCAN_LIMIT)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = camino::Utf8PathBuf::from_path_buf(entry.path()).ok()?;
            if path.extension()? != "md" {
                return None;
            }
            let name = path.file_stem()?.to_string();
            Some(DesktopCommandRow {
                label: format!("/{name}"),
                name,
                path: path.to_string(),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows.truncate(DESKTOP_COMMAND_ROW_LIMIT);
    rows
}

fn format_transcript_text(state: &AppState) -> String {
    if state.transcript_entries.is_empty() {
        return "履歴はまだありません。".to_string();
    }
    state
        .transcript_entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let heading = entry_heading(entry.kind, &entry.title);
            let body = entry.body.trim();
            let step = index + 1;
            if body.is_empty() {
                format!("[{step:02}] {heading}")
            } else {
                format!("[{step:02}] {heading}\n{body}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
fn transcript_rows(state: &AppState) -> Vec<DesktopTranscriptRow> {
    transcript_rows_with_context(state, None, &[])
}

fn transcript_rows_with_context(
    state: &AppState,
    session: Option<&SessionRecord>,
    file_changes: &[DesktopFileChangeRow],
) -> Vec<DesktopTranscriptRow> {
    let base_rows = if state.transcript_entries.is_empty() {
        vec![desktop_transcript_row(
            DesktopTranscriptRowKind::EmptyPlaceholder,
            "00".to_string(),
            "履歴はまだありません".to_string(),
            "依頼を送信すると、ユーザー入力、ツール実行、ファイル変更、最終応答がここに並びます。"
                .to_string(),
            Vec::new(),
        )]
    } else {
        state
            .transcript_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let row_kind = transcript_row_kind_from_entry(entry.kind);
                if is_internal_transcript_projection(row_kind, &entry.title) {
                    return None;
                }
                Some(desktop_transcript_row(
                    row_kind,
                    format!("{:02}", index + 1),
                    entry_heading(entry.kind, &entry.title),
                    entry.body.trim().to_string(),
                    Vec::new(),
                ))
            })
            .collect::<Vec<_>>()
    };

    let terminal = state.run_status.is_terminal()
        || state
            .last_summary
            .as_ref()
            .map(|summary| session_status_is_terminal(summary.status()))
            .unwrap_or(false)
        || session
            .map(|session| session_status_is_terminal(session.status))
            .unwrap_or(false);
    let work_summary = work_summary_row(state, file_changes);
    let mut rows = fold_intermediate_assistant_rows(
        base_rows,
        state,
        file_changes,
        work_summary.is_some(),
        terminal,
    );
    if let Some(work_summary) = work_summary {
        let latest_user_index = rows
            .iter()
            .rposition(|row| row.row_kind == DesktopTranscriptRowKind::User);
        let insert_index = rows
            .iter()
            .enumerate()
            .skip(latest_user_index.map_or(0, |index| index.saturating_add(1)))
            .find_map(|(index, row)| {
                (row.row_kind == DesktopTranscriptRowKind::Assistant).then_some(index)
            })
            .unwrap_or(rows.len());
        rows.insert(insert_index, work_summary);
    }
    renumber_rows(rows)
}

fn fold_intermediate_assistant_rows(
    rows: Vec<DesktopTranscriptRow>,
    state: &AppState,
    file_changes: &[DesktopFileChangeRow],
    has_work_summary: bool,
    terminal: bool,
) -> Vec<DesktopTranscriptRow> {
    let should_fold = terminal
        && has_work_summary
        && (!state.tool_statuses.is_empty()
            || !file_changes.is_empty()
            || state.last_summary.is_some());
    if !should_fold {
        return rows;
    }
    let mut folded = Vec::with_capacity(rows.len());
    let mut retained_assistant_for_turn = false;
    for row in rows.into_iter().rev() {
        if row.row_kind == DesktopTranscriptRowKind::User {
            retained_assistant_for_turn = false;
            folded.push(row);
            continue;
        }
        if row.row_kind == DesktopTranscriptRowKind::Assistant {
            if row.body.trim().is_empty() {
                continue;
            }
            if retained_assistant_for_turn {
                continue;
            }
            retained_assistant_for_turn = true;
        }
        folded.push(row);
    }
    folded.reverse();
    folded
}

#[cfg(test)]
fn completed_run_summary_fixture(
    session_id: SessionId,
    tool_call_count: usize,
    change_count: usize,
) -> crate::session::RunSummary {
    crate::session::RunSummary::from_terminal(
        session_id,
        crate::protocol::TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count,
            failed_tool_count: 0,
            change_count,
            metrics: Default::default(),
        },
    )
}

fn session_status_is_terminal(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
    )
}

fn is_internal_transcript_projection(kind: DesktopTranscriptRowKind, title: &str) -> bool {
    matches!(
        kind,
        DesktopTranscriptRowKind::Tool
            | DesktopTranscriptRowKind::Diff
            | DesktopTranscriptRowKind::ReasoningSummary
            | DesktopTranscriptRowKind::Editing
    ) || matches!(kind, DesktopTranscriptRowKind::System)
        && !title.eq_ignore_ascii_case("User")
        && !title.eq_ignore_ascii_case("Context Compaction")
}

fn work_summary_row(
    state: &AppState,
    file_changes: &[DesktopFileChangeRow],
) -> Option<DesktopTranscriptRow> {
    let has_work = !state.tool_statuses.is_empty()
        || !file_changes.is_empty()
        || state.last_summary.is_some()
        || matches!(state.run_status, RunStatus::Running);
    if !has_work {
        return None;
    }

    let kind = match state.run_status {
        RunStatus::Running => DesktopTranscriptRowKind::WorkSummaryRunning,
        RunStatus::Completed => DesktopTranscriptRowKind::WorkSummaryCompleted,
        RunStatus::Failed => DesktopTranscriptRowKind::WorkSummaryFailed,
        RunStatus::Cancelled => DesktopTranscriptRowKind::WorkSummaryCancelled,
        RunStatus::Idle => DesktopTranscriptRowKind::WorkSummaryIncomplete,
    };
    Some(desktop_transcript_row(
        kind,
        String::new(),
        work_summary_title(state),
        work_summary_body(state, file_changes),
        Vec::new(),
    ))
}

fn work_summary_title(state: &AppState) -> String {
    let elapsed = state
        .last_summary
        .as_ref()
        .and_then(|summary| summary.metrics().elapsed_ms)
        .map(format_duration);
    match state.run_status {
        RunStatus::Running => "作業中".to_string(),
        RunStatus::Failed => elapsed
            .map(|value| format!("{value}で失敗しました"))
            .unwrap_or_else(|| "失敗しました".to_string()),
        RunStatus::Cancelled => elapsed
            .map(|value| format!("{value}で停止しました"))
            .unwrap_or_else(|| "停止しました".to_string()),
        RunStatus::Completed => elapsed
            .map(|value| format!("{value}作業しました"))
            .unwrap_or_else(|| "作業しました".to_string()),
        RunStatus::Idle => "状態未確定の作業履歴".to_string(),
    }
}

fn format_duration(elapsed_ms: u64) -> String {
    let total_seconds = elapsed_ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn work_summary_body(state: &AppState, file_changes: &[DesktopFileChangeRow]) -> String {
    let mut sections = Vec::new();
    if matches!(state.run_status, RunStatus::Idle) {
        sections.push("### 作業サマリ\n- 結果: この turn の完了状態は未確定です。".to_string());
    } else if state.last_summary.is_some() || state.run_status.is_terminal() {
        sections.push(format!(
            "### 作業サマリ\n{}",
            current_run_summary_text(state, file_changes)
        ));
    }
    if matches!(state.run_status, RunStatus::Running) {
        sections.push(format!(
            "### 現在\n- フェーズ: {}\n- 手順: {}\n- モデル要求: {}",
            desktop_run_phase_label(state.progress.current_phase),
            state.progress.active_step,
            state.progress.model_requests
        ));
    }
    if !state.tool_statuses.is_empty() {
        sections.push(format!(
            "### ツール\n{}\n{}",
            format_command_summary_title(&state.tool_statuses),
            format_compact_tool_rows(&state.tool_statuses)
        ));
    }
    if !file_changes.is_empty() {
        sections.push(format!(
            "### 変更\n{}",
            file_changes
                .iter()
                .take(8)
                .map(|row| format!("- [{}] {}", row.action, row.path))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let Some(summary) = &state.last_summary
        && !matches!(state.run_status, RunStatus::Idle)
    {
        sections.push(format!(
            "### 完了\n- 状態: {}\n- ツール: {}件実行 / {}件失敗\n- ファイル変更: {}件",
            format_session_status(summary.status()),
            summary.tool_call_count(),
            summary.failed_tool_count(),
            summary.change_count()
        ));
    }
    if sections.is_empty() {
        "作業内容を整理しています。".to_string()
    } else {
        sections.join("\n\n")
    }
}

fn current_run_summary_text(state: &AppState, file_changes: &[DesktopFileChangeRow]) -> String {
    let mut lines = Vec::new();
    let status = state
        .last_summary
        .as_ref()
        .map(|summary| format_session_status(summary.status()).to_string())
        .unwrap_or_else(|| run_status_label(state.run_status).to_string());
    lines.push(format!("- 状態: {status}"));
    if !file_changes.is_empty() {
        lines.push(format!(
            "- 変更: {}件 ({})",
            file_changes.len(),
            file_changes
                .iter()
                .take(4)
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !state.tool_statuses.is_empty() {
        let completed = state
            .tool_statuses
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Completed)
            .count();
        let failed = state
            .tool_statuses
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Failed)
            .count();
        let declined = state
            .tool_statuses
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Declined)
            .count();
        let cancelled = state
            .tool_statuses
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Cancelled)
            .count();
        let mut counts = vec![format!("{completed}件完了")];
        if declined > 0 {
            counts.push(format!("{declined}件拒否"));
        }
        if cancelled > 0 {
            counts.push(format!("{cancelled}件キャンセル"));
        }
        if failed > 0 {
            counts.push(format!("{failed}件失敗"));
        }
        lines.push(format!("- コマンド/ツール: {}", counts.join(" / ")));
    }
    if let Some(last_tool) = state.tool_statuses.last()
        && let Some(summary) = last_tool.summary.as_ref().or(last_tool.error.as_ref())
        && !summary.trim().is_empty()
    {
        lines.push(format!("- 直近出力: {}", single_line_preview(summary, 180)));
    }
    if lines.len() == 1 {
        lines.push("- 詳細は作業履歴に記録されています。".to_string());
    }
    lines.join("\n")
}

fn format_compact_tool_rows(tools: &[crate::tui::state::ToolStatusView]) -> String {
    tools
        .iter()
        .take(8)
        .map(|tool| {
            let mut line = format!("- [{}] {}", tool_call_status_label(tool.status), tool.title);
            if let Some(summary) = tool.summary.as_ref().or(tool.error.as_ref())
                && !summary.trim().is_empty()
            {
                line.push_str(&format!("\n  出力: {}", single_line_preview(summary, 220)));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn renumber_rows(mut rows: Vec<DesktopTranscriptRow>) -> Vec<DesktopTranscriptRow> {
    for (index, row) in rows.iter_mut().enumerate() {
        row.step = format!("{:02}", index + 1);
    }
    rows
}

fn format_command_summary_title(tools: &[crate::tui::state::ToolStatusView]) -> String {
    let completed = tools
        .iter()
        .filter(|tool| tool.status == ToolCallStatus::Completed)
        .count();
    let failed = tools
        .iter()
        .filter(|tool| tool.status == ToolCallStatus::Failed)
        .count();
    let declined = tools
        .iter()
        .filter(|tool| tool.status == ToolCallStatus::Declined)
        .count();
    let cancelled = tools
        .iter()
        .filter(|tool| tool.status == ToolCallStatus::Cancelled)
        .count();
    let running = tools
        .iter()
        .filter(|tool| {
            matches!(
                tool.status,
                ToolCallStatus::Pending | ToolCallStatus::Running
            )
        })
        .count();
    let mut parts = vec![format!("{completed}件のコマンドを実行")];
    if running > 0 {
        parts.push(format!("{running}件実行中"));
    }
    if declined > 0 {
        parts.push(format!("{declined}件拒否"));
    }
    if cancelled > 0 {
        parts.push(format!("{cancelled}件キャンセル"));
    }
    if failed > 0 {
        parts.push(format!("{failed}件失敗"));
    }
    parts.join(", ")
}

fn transcript_row_kind_from_entry(kind: TranscriptKind) -> DesktopTranscriptRowKind {
    match kind {
        TranscriptKind::User => DesktopTranscriptRowKind::User,
        TranscriptKind::Assistant => DesktopTranscriptRowKind::Assistant,
        TranscriptKind::ReasoningSummary => DesktopTranscriptRowKind::ReasoningSummary,
        TranscriptKind::Editing => DesktopTranscriptRowKind::Editing,
        TranscriptKind::Tool => DesktopTranscriptRowKind::Tool,
        TranscriptKind::Diff => DesktopTranscriptRowKind::Diff,
        TranscriptKind::System => DesktopTranscriptRowKind::System,
        TranscriptKind::Error => DesktopTranscriptRowKind::Error,
    }
}

fn format_tool_status_text(state: &AppState) -> String {
    let mut lines = Vec::new();
    if state.tool_statuses.is_empty() {
        lines.push("ツール: 実行履歴はまだありません。".to_string());
    } else {
        lines.push("ツール:".to_string());
        lines.extend(state.tool_statuses.iter().map(|tool| {
            let summary = tool.summary.clone().unwrap_or_default();
            if summary.is_empty() {
                format!(
                    "- {} [{}]",
                    tool.title,
                    format!("{:?}", tool.status).to_lowercase()
                )
            } else {
                format!(
                    "- {} [{}] {}",
                    tool.title,
                    format!("{:?}", tool.status).to_lowercase(),
                    summary
                )
            }
        }));
    }
    lines.join("\n")
}

fn append_replay_summary(tool_status_text: &mut String, report: &ReplayReport) {
    if !tool_status_text.is_empty() {
        tool_status_text.push_str("\n\n");
    }
    tool_status_text.push_str("リプレイ:\n");
    tool_status_text.push_str(&format!(
        "- status: {}",
        format!("{:?}", report.status).to_lowercase()
    ));
    if let Some(owner) = report.primary_owner {
        tool_status_text.push_str(&format!(
            "\n- 主担当: {}",
            format!("{:?}", owner).to_lowercase()
        ));
    }
    if !report.summary.trim().is_empty() {
        tool_status_text.push_str(&format!("\n- サマリ: {}", report.summary.trim()));
    }
    if let Some(restart) = &report.restart_point {
        tool_status_text.push_str(&format!("\n- 再開点: {restart}"));
    }
}

fn format_run_status_text(state: &AppState) -> String {
    let mut lines = vec![run_status_label(state.run_status).to_string()];
    if let Some(message) = &state.status_message {
        lines.push(format!("状態: {message}"));
    }
    lines.join("\n")
}

fn format_progress_text(state: &AppState) -> String {
    let progress = &state.progress;
    vec![
        progress.status.clone(),
        format!(
            "フェーズ: {}",
            desktop_run_phase_label(progress.current_phase)
        ),
        format!("手順: {}", progress.active_step),
        format!("モデル要求: {}", progress.model_requests),
        format!(
            "ツール: {}件開始 / {}件完了 / {}件拒否 / {}件キャンセル / {}件失敗",
            progress.tool_calls_started,
            progress.tool_calls_completed,
            progress.tool_calls_declined,
            progress.tool_calls_cancelled,
            progress.tool_calls_failed
        ),
        format!("圧縮: {}", progress.compactions),
    ]
    .join("\n")
}

pub(crate) const fn desktop_run_phase_label(phase: RunProgressPhase) -> &'static str {
    match phase {
        RunProgressPhase::Ready => "待機",
        RunProgressPhase::Session => "セッション開始",
        RunProgressPhase::User => "入力受付",
        RunProgressPhase::Context => "コンテキスト更新",
        RunProgressPhase::Model => "モデル応答",
        RunProgressPhase::Provider(crate::llm::ProviderPhase::AttemptStarted) => "Provider要求開始",
        RunProgressPhase::Provider(crate::llm::ProviderPhase::RequestInFlight) => {
            "Provider要求処理中"
        }
        RunProgressPhase::Provider(crate::llm::ProviderPhase::HeadersReceived) => {
            "Provider応答ヘッダー受信"
        }
        RunProgressPhase::Provider(crate::llm::ProviderPhase::FirstProgress) => {
            "Provider応答受信中"
        }
        RunProgressPhase::Provider(crate::llm::ProviderPhase::LastProgress) => {
            "Provider最終応答受信"
        }
        RunProgressPhase::Provider(crate::llm::ProviderPhase::ProviderTerminal) => "Provider完了",
        RunProgressPhase::Permission => "確認",
        RunProgressPhase::Tool => "ツール実行",
        RunProgressPhase::Compaction => "圧縮",
        RunProgressPhase::RuntimeFeedback => "実行フィードバック",
        RunProgressPhase::StopRequested => "停止処理",
        RunProgressPhase::Terminal => "終了処理",
        RunProgressPhase::Loaded => "履歴読込",
    }
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "待機中",
        RunStatus::Running => "実行中",
        RunStatus::Completed => "完了",
        RunStatus::Cancelled => "停止済み",
        RunStatus::Failed => "失敗",
    }
}

fn entry_heading(kind: TranscriptKind, title: &str) -> String {
    match kind {
        TranscriptKind::User => "ユーザー依頼".to_string(),
        TranscriptKind::Assistant => "応答".to_string(),
        TranscriptKind::ReasoningSummary => "推論要約".to_string(),
        TranscriptKind::Editing => "編集中".to_string(),
        TranscriptKind::Tool => format!("コマンド / ツール - {title}"),
        TranscriptKind::Diff => format!("ファイル変更 - {title}"),
        TranscriptKind::System => format!("システム - {title}"),
        TranscriptKind::Error => format!("エラー - {title}"),
    }
}

fn tool_call_status_label(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "待機中",
        ToolCallStatus::Running => "実行中",
        ToolCallStatus::Completed => "完了",
        ToolCallStatus::Declined => "拒否",
        ToolCallStatus::Cancelled => "キャンセル",
        ToolCallStatus::Failed => "失敗",
    }
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
    use crate::protocol::{FileChangeEvidence, TurnItem, TurnItemPayload};
    use crate::session::{ChangeKind, SessionStatus};
    use camino::Utf8PathBuf;

    fn test_session_record(title: &str) -> SessionRecord {
        SessionRecord {
            id: SessionId::new(),
            project_id: ProjectId::new(),
            title: title.to_string(),
            status: SessionStatus::Completed,
            cwd: Utf8PathBuf::from(format!("C:/workspace/{title}")),
            model: "test-model".to_string(),
            base_url: "http://127.0.0.1:1234".to_string(),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: crate::session::SessionModelParameters::default(),
            created_at_ms: 1_000,
            updated_at_ms: 6_000,
            completed_at_ms: Some(6_000),
        }
    }

    fn session_record(project_id: ProjectId, title: &str) -> SessionRecord {
        let mut session = test_session_record(title);
        session.project_id = project_id;
        session.created_at_ms = 1;
        session.updated_at_ms = 2;
        session.completed_at_ms = Some(2);
        session
    }

    fn canonical_read_with_elapsed(
        session: &SessionRecord,
        turn_items: Vec<TurnItem>,
        turn_elapsed_ms: std::collections::HashMap<crate::protocol::TurnId, u64>,
    ) -> CanonicalSessionRead {
        let latest_turn_id = turn_items.last().map(|item| item.turn_id);
        CanonicalSessionRead {
            session: session.clone(),
            history: crate::session::CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: usize::MAX,
                total: 0,
                has_more: false,
                items: Vec::new(),
            },
            turns: crate::session::CanonicalTurnPage {
                session: session.clone(),
                offset: 0,
                limit: DESKTOP_TURN_PAGE_LIMIT,
                total: turn_items.len(),
                has_more: false,
                items: turn_items,
            },
            turn_elapsed_ms,
            latest_turn_id,
            active_turn_id: None,
            active_turn_sequence_no: None,
        }
    }

    #[test]
    fn terminal_projection_uses_the_typed_outcome_as_its_only_owner() {
        let session = session_record(ProjectId::new(), "typed terminal");

        let interrupted = transcript_rows_from_turn_items_with_context(
            &session,
            &[TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id: crate::protocol::TurnId::new(),
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                        cause: crate::protocol::TurnInterruptionCause::ApprovalAborted,
                    },
                },
            }],
        );
        let interrupted_summary = interrupted
            .iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCancelled)
            .expect("typed interrupted row");
        assert!(interrupted_summary.body.contains("指示を入力してください"));

        let failed = transcript_rows_from_turn_items_with_context(
            &session,
            &[TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id: crate::protocol::TurnId::new(),
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Failed {
                        error: "permission approval aborted by user".to_string(),
                    },
                },
            }],
        );
        let failed_summary = failed
            .iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryFailed)
            .expect("typed failed row");
        assert!(
            failed_summary
                .body
                .contains("permission approval aborted by user")
        );
        assert!(!failed_summary.body.contains("指示を入力してください"));
    }

    #[test]
    fn missing_terminal_never_derives_a_terminal_from_session_aggregate() {
        let mut session = session_record(ProjectId::new(), "missing terminal");
        session.status = SessionStatus::Completed;
        let session_id = session.id;
        let first_turn = crate::protocol::TurnId::new();
        let second_turn = crate::protocol::TurnId::new();
        let items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: first_turn,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "first".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: first_turn,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "first tool".to_string(),
                    summary: "done".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: second_turn,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::UserMessage {
                    text: "second".to_string(),
                },
            },
        ];

        let rows = transcript_rows_from_turn_items_with_context(&session, &items);
        let summaries = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.row_kind,
                    DesktopTranscriptRowKind::WorkSummaryIncomplete
                        | DesktopTranscriptRowKind::WorkSummaryCompleted
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(summaries.len(), 2);
        assert_eq!(
            summaries[0].row_kind,
            DesktopTranscriptRowKind::WorkSummaryIncomplete
        );
        assert!(summaries[0].body.contains("完了状態は未確定"));
        assert_eq!(
            summaries[1].row_kind,
            DesktopTranscriptRowKind::WorkSummaryIncomplete
        );
        assert!(summaries[1].body.contains("完了状態は未確定"));
    }

    #[test]
    fn missing_terminal_only_projects_running_from_session_lifecycle() {
        let group = TurnTranscriptGroup::default();
        assert_eq!(
            turn_work_summary_kind(&group, Some(SessionStatus::Running)),
            DesktopTranscriptRowKind::WorkSummaryRunning
        );
        for status in [
            SessionStatus::Idle,
            SessionStatus::Completed,
            SessionStatus::Cancelled,
            SessionStatus::Failed,
        ] {
            assert_eq!(
                turn_work_summary_kind(&group, Some(status)),
                DesktopTranscriptRowKind::WorkSummaryIncomplete
            );
            assert!(turn_summary_text(&group, &[], Some(status)).contains("完了状態は未確定"));
        }
        assert_eq!(
            turn_work_summary_kind(&group, None),
            DesktopTranscriptRowKind::WorkSummaryIncomplete
        );
    }

    #[test]
    fn work_summary_stable_identity_survives_running_to_completed_projection() {
        let mut running_session = session_record(ProjectId::new(), "stable summary identity");
        running_session.status = SessionStatus::Running;
        running_session.completed_at_ms = None;
        let turn_id = crate::protocol::TurnId::new();
        let mut items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: running_session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "inspect".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: running_session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "inspect".to_string(),
                    summary: "done".to_string(),
                },
            },
        ];
        let running_rows = transcript_rows_from_turn_items_with_context(&running_session, &items);
        let running_summary = running_rows
            .iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning)
            .expect("running work summary");

        let mut completed_session = running_session.clone();
        completed_session.status = SessionStatus::Completed;
        completed_session.completed_at_ms = Some(completed_session.updated_at_ms);
        items.push(TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id: completed_session.id,
            turn_id,
            source_item_id: None,
            sequence_no: 3,
            payload: TurnItemPayload::Terminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
            },
        });
        let completed_rows =
            transcript_rows_from_turn_items_with_context(&completed_session, &items);
        let completed_summary = completed_rows
            .iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
            .expect("completed work summary");

        assert_ne!(completed_summary.title, running_summary.title);
        assert_ne!(completed_summary.body, running_summary.body);
        assert_eq!(
            completed_summary.stable_history_identity,
            running_summary.stable_history_identity,
        );
        assert_eq!(
            completed_summary.stable_history_identity.as_deref(),
            Some(format!("turn:{turn_id}:work-summary").as_str()),
        );
    }

    #[test]
    fn same_turn_steer_segments_have_unique_durable_work_summary_identities() {
        let session = session_record(ProjectId::new(), "stable steer segments");
        let turn_id = crate::protocol::TurnId::new();
        let steer_id = crate::protocol::TurnItemId::new();
        let items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "inspect".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "inspect".to_string(),
                    summary: "first segment".to_string(),
                },
            },
            TurnItem {
                id: steer_id,
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::SteerMessage {
                    text: "also verify".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 4,
                payload: TurnItemPayload::AgentMessage {
                    text: "done".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 5,
                payload: TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            },
        ];

        let identities = transcript_rows_from_turn_items_with_context(&session, &items)
            .into_iter()
            .filter(|row| {
                matches!(
                    row.row_kind,
                    DesktopTranscriptRowKind::WorkSummaryIncomplete
                        | DesktopTranscriptRowKind::WorkSummaryCompleted
                )
            })
            .map(|row| {
                row.stable_history_identity
                    .expect("stable work summary identity")
            })
            .collect::<Vec<_>>();

        assert_eq!(
            identities,
            vec![
                format!("turn:{turn_id}:before:{steer_id}:work-summary"),
                turn_work_summary_stable_identity(turn_id),
            ]
        );
    }

    #[test]
    fn ignored_only_turn_does_not_leak_elapsed_or_identity_across_partial_boundary() {
        let session = session_record(ProjectId::new(), "partial ignored boundary");
        let ignored_turn = crate::protocol::TurnId::new();
        let visible_turn = crate::protocol::TurnId::new();
        let items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id: ignored_turn,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::Plan {
                    explanation: None,
                    plan: Vec::new(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id: visible_turn,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::AgentMessage {
                    text: "visible partial answer".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id: visible_turn,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            },
        ];
        let elapsed =
            std::collections::HashMap::from([(ignored_turn, 1_000), (visible_turn, 91_889)]);

        let summary =
            transcript_rows_from_turn_items_with_context_and_elapsed(&session, &items, &elapsed)
                .into_iter()
                .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
                .expect("visible turn work summary");

        assert_eq!(summary.title, "1m 31s作業しました");
        assert_eq!(
            summary.stable_history_identity.as_deref(),
            Some(turn_work_summary_stable_identity(visible_turn).as_str()),
        );
    }

    #[test]
    fn session_selection_prefers_current_project_without_explicit_session() {
        let current_project = ProjectId::new();
        let other_project = ProjectId::new();
        let sessions = vec![
            session_record(other_project, "other"),
            session_record(current_project, "current"),
        ];

        let selected = select_session_index(&sessions, None, Some(current_project), false).unwrap();

        assert_eq!(selected, 1);
    }

    #[test]
    fn command_snapshot_projection_is_bounded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8");
        let commands = root.join(".moyai").join("commands");
        std::fs::create_dir_all(&commands).expect("command directory");
        for index in 0..(DESKTOP_COMMAND_ROW_LIMIT + 12) {
            std::fs::write(commands.join(format!("command-{index:03}.md")), "# command")
                .expect("command fixture");
        }

        let rows = load_command_rows(&root);

        assert_eq!(rows.len(), DESKTOP_COMMAND_ROW_LIMIT);
        assert!(rows.windows(2).all(|pair| pair[0].name <= pair[1].name));
    }

    #[tokio::test]
    async fn explicit_session_outside_the_sidebar_page_is_inserted_and_selected() {
        use crate::app::AppBootstrap;
        use crate::config::ResolvedConfig;
        use crate::session::{SessionSelector, SessionStartRequest};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let app = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &root,
            store,
            ResolvedConfig::default(),
        )
        .await
        .expect("app");
        let mut created = Vec::new();
        for index in 0..25 {
            let session = app
                .session_service
                .start_or_resume(
                    SessionStartRequest {
                        selector: SessionSelector::New,
                        title: Some(format!("session {index}")),
                        cwd: app.workspace.cwd.clone(),
                        model: app.config.model.model.clone(),
                        base_url: app.config.model.base_url.clone(),
                        access_mode: app.config.permissions.access_mode,
                    },
                    app.workspace.clone(),
                )
                .await
                .expect("session");
            created.push(session.session.id);
        }
        let initial = app
            .session_service
            .loaded_sessions(app.workspace.project_id, 20, false)
            .await
            .expect("initial page");
        let outside_page = created
            .into_iter()
            .find(|session_id| {
                !initial
                    .sessions
                    .iter()
                    .any(|summary| summary.session.id == *session_id)
            })
            .expect("session outside first page");

        let snapshot = load_snapshot_for_selection(&app, Some(outside_page))
            .await
            .expect("explicit snapshot");

        assert_eq!(snapshot.selected_session_id(), Some(outside_page));
        assert!(
            snapshot
                .session_rows
                .iter()
                .any(|row| row.session_id == outside_page)
        );
    }

    #[test]
    fn project_rows_keep_current_workspace_visible() {
        let current_project = ProjectId::new();
        let other_project = ProjectId::new();
        let projects = vec![ProjectRecord {
            id: other_project,
            root_path: Utf8PathBuf::from("C:/workspace/other"),
            display_name: "Workspace".to_string(),
            vcs_kind: "none".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }];

        let (rows, selected) = build_project_rows(
            &projects,
            current_project,
            &Utf8PathBuf::from("C:/workspace/current"),
            &[],
        );

        assert_eq!(rows[selected].project_id, current_project);
        assert_eq!(rows[selected].label, "current");
        assert_eq!(
            rows.iter()
                .find(|row| row.project_id == other_project)
                .map(|row| row.label.as_str()),
            Some("other")
        );
        assert!(rows.iter().any(|row| row.project_id == other_project));
    }

    #[test]
    fn project_rows_hide_quick_chat_workspace() {
        let quick_chat_project = ProjectId::new();
        let normal_project = ProjectId::new();
        let quick_chat_root = Utf8PathBuf::from("C:/data/quick-chat-workspace");
        let projects = vec![
            ProjectRecord {
                id: quick_chat_project,
                root_path: quick_chat_root.clone(),
                display_name: "quick-chat-workspace".to_string(),
                vcs_kind: "none".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            ProjectRecord {
                id: normal_project,
                root_path: Utf8PathBuf::from("C:/workspace/normal"),
                display_name: "normal".to_string(),
                vcs_kind: "none".to_string(),
                created_at_ms: 2,
                updated_at_ms: 2,
            },
        ];

        let (rows, selected) = build_project_rows(
            &projects,
            quick_chat_project,
            &quick_chat_root,
            &[quick_chat_root.clone()],
        );

        assert_eq!(selected, rows.len());
        assert!(!rows.iter().any(|row| row.project_id == quick_chat_project));
        assert!(rows.iter().any(|row| row.project_id == normal_project));
    }

    #[test]
    fn project_rows_hide_legacy_internal_desktop_workspaces() {
        let internal_project = ProjectId::new();
        let normal_project = ProjectId::new();
        let data_dir = Utf8PathBuf::from("C:/Users/example/AppData/Roaming/moyAI");
        let internal_root = data_dir.join("desktop-workspace-after-delete");
        let projects = vec![
            ProjectRecord {
                id: internal_project,
                root_path: internal_root,
                display_name: "desktop-workspace-after-delete".to_string(),
                vcs_kind: "none".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            ProjectRecord {
                id: normal_project,
                root_path: Utf8PathBuf::from("C:/workspace/normal"),
                display_name: "normal".to_string(),
                vcs_kind: "none".to_string(),
                created_at_ms: 2,
                updated_at_ms: 2,
            },
        ];

        let (rows, selected) = build_project_rows(
            &projects,
            normal_project,
            &Utf8PathBuf::from("C:/workspace/normal"),
            &internal_desktop_project_roots(&data_dir),
        );

        assert_eq!(selected, 0);
        assert!(!rows.iter().any(|row| row.project_id == internal_project));
        assert_eq!(rows[0].project_id, normal_project);
    }

    #[test]
    fn file_change_rows_project_canonical_turn_items_into_desktop_artifacts() {
        let session_id = SessionId::new();
        let turn_items = vec![TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id: crate::protocol::TurnId::new(),
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![crate::session::ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: crate::session::ChangeId::new(),
                    kind: ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("src/main.rs")),
                    path_after: Some(Utf8PathBuf::from("src/main.rs")),
                    summary: "updated desktop UI projection".to_string(),
                }],
                summary: "updated desktop UI projection".to_string(),
            },
        }];

        let rows = file_change_rows_from_turn_items(&turn_items);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "main.rs");
        assert_eq!(rows[0].path, "src/main.rs");
        assert_eq!(rows[0].action, "更新");
        assert!(rows[0].summary.contains("desktop UI projection"));
    }

    #[test]
    fn transcript_rows_keep_file_changes_inside_each_user_turn() {
        let session = session_record(ProjectId::new(), "multi-turn");
        let session_id = session.id;
        let turn_a = crate::protocol::TurnId::new();
        let turn_b = crate::protocol::TurnId::new();
        let turn_items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: turn_a,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "指示プロンプトA".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: turn_a,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Write,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "Updated docs/workflow-notes.md".to_string(),
                    summary: "Command: write\n\nStdout:\nupdated docs/workflow-notes.md"
                        .to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: turn_a,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::FileChange {
                    call_id: crate::session::ToolCallId::new(),
                    change_ids: vec![crate::session::ChangeId::new()],
                    changes: vec![FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("docs/workflow-notes.md")),
                        path_after: Some(Utf8PathBuf::from("docs/workflow-notes.md")),
                        summary: "A change".to_string(),
                    }],
                    summary: "A change".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: turn_a,
                source_item_id: None,
                sequence_no: 4,
                payload: TurnItemPayload::AgentMessage {
                    text: "応答A".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: turn_b,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "指示プロンプトB".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: turn_b,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::FileChange {
                    call_id: crate::session::ToolCallId::new(),
                    change_ids: vec![crate::session::ChangeId::new()],
                    changes: vec![FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.contract")),
                        summary: "B change".to_string(),
                    }],
                    summary: "B change".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: turn_b,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::AgentMessage {
                    text: "応答B".to_string(),
                },
            },
        ];

        let rows = transcript_rows_from_turn_items_with_context(&session, &turn_items);
        let index_user_a = rows
            .iter()
            .position(|row| {
                row.row_kind == DesktopTranscriptRowKind::User
                    && row.body.contains("指示プロンプトA")
            })
            .expect("user A row");
        let index_change_a = rows
            .iter()
            .position(|row| {
                row.row_kind == DesktopTranscriptRowKind::FileChanges
                    && row.body.contains("docs/workflow-notes.md")
            })
            .expect("file change A row");
        let index_assistant_a = rows
            .iter()
            .position(|row| {
                row.row_kind == DesktopTranscriptRowKind::Assistant && row.body.contains("応答A")
            })
            .expect("assistant A row");
        let index_user_b = rows
            .iter()
            .position(|row| {
                row.row_kind == DesktopTranscriptRowKind::User
                    && row.body.contains("指示プロンプトB")
            })
            .expect("user B row");
        let index_change_b = rows
            .iter()
            .position(|row| {
                row.row_kind == DesktopTranscriptRowKind::FileChanges
                    && row.body.contains("tests/workflow.contract")
            })
            .expect("file change B row");
        let index_assistant_b = rows
            .iter()
            .position(|row| {
                row.row_kind == DesktopTranscriptRowKind::Assistant && row.body.contains("応答B")
            })
            .expect("assistant B row");

        assert!(index_user_a < index_assistant_a);
        assert!(index_assistant_a < index_change_a);
        assert!(index_user_a < index_change_a);
        assert!(index_change_a < index_user_b);
        assert!(index_user_b < index_assistant_b);
        assert!(index_assistant_b < index_change_b);
        assert!(index_user_b < index_change_b);
        assert_eq!(
            rows.iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::FileChanges)
                .count(),
            2
        );
        assert_eq!(rows[index_change_a].file_changes.len(), 1);
        assert_eq!(rows[index_change_a].file_changes[0].action, "更新");
        assert_eq!(
            rows[index_change_a].file_changes[0].path,
            "docs/workflow-notes.md"
        );
        assert_eq!(rows[index_change_b].file_changes.len(), 1);
        assert_eq!(rows[index_change_b].file_changes[0].action, "追加");
        assert_eq!(
            rows[index_change_b].file_changes[0].path,
            "tests/workflow.contract"
        );
        assert!(
            rows.iter()
                .filter(|row| {
                    matches!(
                        row.row_kind,
                        DesktopTranscriptRowKind::WorkSummaryRunning
                            | DesktopTranscriptRowKind::WorkSummaryIncomplete
                            | DesktopTranscriptRowKind::WorkSummaryCompleted
                            | DesktopTranscriptRowKind::WorkSummaryFailed
                            | DesktopTranscriptRowKind::WorkSummaryCancelled
                    )
                })
                .count()
                >= 2
        );
    }

    #[test]
    fn file_change_rows_normalize_workspace_paths_and_collapse_session_edits() {
        let session_id = SessionId::new();
        let workspace_root = Utf8PathBuf::from("C:/workspace/workflow");
        let turn_items = vec![TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id: crate::protocol::TurnId::new(),
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![crate::session::ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from(
                            "C:/workspace/workflow/src/workflow.rs",
                        )),
                        path_after: Some(Utf8PathBuf::from(
                            "C:/workspace/workflow/src/workflow.rs",
                        )),
                        summary: "Updated src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.contract")),
                        summary: "Added tests/workflow.contract".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from(
                            "C:/workspace/workflow/tests/workflow.contract",
                        )),
                        path_after: Some(Utf8PathBuf::from(
                            "C:/workspace/workflow/tests/workflow.contract",
                        )),
                        summary: "Updated tests/workflow.contract".to_string(),
                    },
                ],
                summary: "Updated files".to_string(),
            },
        }];

        let rows = file_change_rows_from_turn_items_with_root(&turn_items, Some(&workspace_root));

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, "src/workflow.rs");
        assert_eq!(rows[0].action, "追加");
        assert_eq!(rows[0].summary, "Updated src/workflow.rs");
        assert_eq!(rows[1].path, "tests/workflow.contract");
        assert_eq!(rows[1].action, "追加");
        assert_eq!(rows[1].summary, "Updated tests/workflow.contract");
    }

    #[test]
    fn file_change_rows_preserve_runtime_cache_files_in_user_history() {
        let session_id = SessionId::new();
        let turn_items = vec![TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id: crate::protocol::TurnId::new(),
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![crate::session::ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from(
                            "build-artifacts/cache/workflow.snapshot",
                        )),
                        path_after: Some(Utf8PathBuf::from(
                            "build-artifacts/cache/workflow.snapshot",
                        )),
                        summary: "Updated runtime cache".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Updated workflow logic".to_string(),
                    },
                ],
                summary: "Updated files".to_string(),
            },
        }];

        let rows = file_change_rows_from_turn_items(&turn_items);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, "build-artifacts/cache/workflow.snapshot");
        assert_eq!(rows[1].path, "src/workflow.rs");
    }

    #[test]
    fn artifact_rows_preserve_runtime_cache_files() {
        let rows = vec![
            DesktopFileChangeRow {
                label: "workflow.rs".to_string(),
                path: "src/workflow.rs".to_string(),
                kind: crate::session::ChangeKind::Add,
                action: "add".to_string(),
                summary: String::new(),
                tool_call_ids: vec![crate::session::ToolCallId::new()],
            },
            DesktopFileChangeRow {
                label: "workflow.snapshot".to_string(),
                path: "build-artifacts/cache/workflow.snapshot".to_string(),
                kind: crate::session::ChangeKind::Add,
                action: "add".to_string(),
                summary: String::new(),
                tool_call_ids: vec![crate::session::ToolCallId::new()],
            },
        ];

        let artifacts = artifact_rows_from_file_changes(&rows);

        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].path, "build-artifacts/cache/workflow.snapshot");
        assert_eq!(artifacts[1].path, "src/workflow.rs");
    }

    #[test]
    fn transcript_text_projects_chat_events_as_scannable_sections() {
        let mut state = AppState::default();
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "create src/workflow.rs".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Tool,
                title: "write".to_string(),
                body: "src/workflow.rs [Completed]".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Diff,
                title: "File changes".to_string(),
                body: "Added src/workflow.rs".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Context Compaction".to_string(),
                body: "圧縮しました\n\nCompactionContinuity".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let text = format_transcript_text(&state);

        assert!(text.contains("[01] ユーザー依頼"));
        assert!(text.contains("[02] コマンド / ツール - write"));
        assert!(text.contains("[03] ファイル変更 - File changes"));
        assert!(!text.contains("===="));
        let rows = transcript_rows(&state);
        let compaction = rows
            .iter()
            .find(|row| row.title == "システム - Context Compaction")
            .expect("context compaction should remain visible in Desktop transcript");
        assert!(compaction.body.contains("圧縮しました"));
        assert!(compaction.body.contains("CompactionContinuity"));

        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];
        let rows = transcript_rows(&state);
        let summary = rows
            .iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryIncomplete)
            .expect("tool status should be projected into a work summary");
        assert!(summary.body.contains("1件のコマンドを実行"));
        assert!(
            !rows
                .iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::Tool)
        );

        state.run_status = RunStatus::Running;
        state.progress.active_step = "Running verify-contract --behavior".to_string();
        state.progress.current_phase = RunProgressPhase::Tool;
        state.current_plan = Some(crate::tui::state::PlanView {
            explanation: Some("canonical plan".to_string()),
            steps: vec![crate::protocol::PlanStep {
                step: "contract verificationを実行".to_string(),
                status: crate::protocol::PlanStepStatus::InProgress,
            }],
        });
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 1, 1));

        let rows = transcript_rows(&state);
        let summary = rows
            .iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryRunning)
            .expect("running state should be projected into an expanded work summary");
        assert_eq!(summary.title, "作業中");
        assert!(!summary.body.contains("canonical plan"));
        assert!(!summary.body.contains("contract verificationを実行"));
        assert!(summary.body.contains("完了サマリ") || summary.body.contains("### 完了"));
        assert!(!rows.iter().any(|row| row.title == "完了サマリ"));
    }

    #[test]
    fn live_idle_work_summary_remains_unconfirmed() {
        let mut state = AppState::default();
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 1, 0));
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        let row = work_summary_row(&state, &[]).expect("idle work summary");

        assert_eq!(
            row.row_kind,
            DesktopTranscriptRowKind::WorkSummaryIncomplete
        );
        assert_eq!(row.title, "状態未確定の作業履歴");
        assert!(row.body.contains("完了状態は未確定"));
        assert!(!row.body.contains("### 完了"));
        assert!(!row.body.contains("- 状態: 完了"));
    }

    #[test]
    fn live_work_summary_preserves_each_typed_run_status() {
        let mut state = AppState::default();
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        for (status, expected) in [
            (
                RunStatus::Running,
                DesktopTranscriptRowKind::WorkSummaryRunning,
            ),
            (
                RunStatus::Completed,
                DesktopTranscriptRowKind::WorkSummaryCompleted,
            ),
            (
                RunStatus::Cancelled,
                DesktopTranscriptRowKind::WorkSummaryCancelled,
            ),
            (
                RunStatus::Failed,
                DesktopTranscriptRowKind::WorkSummaryFailed,
            ),
            (
                RunStatus::Idle,
                DesktopTranscriptRowKind::WorkSummaryIncomplete,
            ),
        ] {
            state.run_status = status;
            assert_eq!(
                work_summary_row(&state, &[])
                    .expect("live work summary")
                    .row_kind,
                expected
            );
        }
    }

    #[test]
    fn live_completed_work_summary_uses_terminal_elapsed_not_session_elapsed() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(crate::session::RunSummary::from_terminal(
            SessionId::new(),
            crate::protocol::TurnId::new(),
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: crate::session::RunMetrics {
                    elapsed_ms: Some(50_970),
                    ..Default::default()
                },
            },
        ));
        let mut session = session_record(ProjectId::new(), "live terminal elapsed");
        session.created_at_ms = 1_000;
        session.updated_at_ms = 908_000;
        session.completed_at_ms = Some(908_000);

        let row = transcript_rows_with_context(&state, Some(&session), &[])
            .into_iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
            .expect("completed work summary");

        assert_eq!(row.title, "50s作業しました");
    }

    #[test]
    fn stored_turn_work_summary_uses_terminal_elapsed_time_and_hides_internal_rows() {
        let project_id = ProjectId::new();
        let mut session = session_record(project_id, "workflow");
        session.created_at_ms = 1_000;
        session.updated_at_ms = 908_000;
        session.completed_at_ms = Some(908_000);
        let turn_id = crate::protocol::TurnId::new();
        let turn_items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "make a workflow artifact".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "verify-contract --behavior".to_string(),
                    summary: "Command: verify-contract --behavior\n\nStdout:\nOK".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Write,
                    status: crate::protocol::ToolLifecycleStatus::Pending,
                    title: "write".to_string(),
                    summary: String::new(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 4,
                payload: TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 5,
                payload: TurnItemPayload::AgentMessage {
                    text: "src/workflow.rsを追加しました。".to_string(),
                },
            },
        ];

        let read = canonical_read_with_elapsed(
            &session,
            turn_items.clone(),
            std::collections::HashMap::from([(turn_id, 107_000)]),
        );
        let detail = build_session_detail(&read, None);

        assert!(detail.transcript_rows.iter().any(|row| {
            row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted
                && row.title == "1m 47s作業しました"
        }));
        assert!(!detail.transcript_rows.iter().any(|row| {
            row.title.contains("Terminal")
                || row.title.contains("編集中")
                || row.row_kind == DesktopTranscriptRowKind::Tool
                || row.row_kind == DesktopTranscriptRowKind::Editing
        }));
        assert!(detail.transcript_rows.iter().any(|row| {
            row.row_kind == DesktopTranscriptRowKind::Assistant
                && row.body.contains("src/workflow.rs")
        }));

        let legacy = build_session_detail(
            &canonical_read_with_elapsed(&session, turn_items, Default::default()),
            None,
        );
        assert!(legacy.transcript_rows.iter().any(|row| {
            row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted
                && row.title == "作業履歴 / 作業サマリ"
        }));
    }

    #[test]
    fn completed_turn_durations_remain_stable_after_later_turn_and_compaction() {
        let session = session_record(ProjectId::new(), "stable durations");
        let turn_a = crate::protocol::TurnId::new();
        let turn_b = crate::protocol::TurnId::new();
        let turn_c = crate::protocol::TurnId::new();
        let item = |turn_id, sequence_no, payload| TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id: session.id,
            turn_id,
            source_item_id: None,
            sequence_no,
            payload,
        };
        let initial_items = vec![
            item(
                turn_a,
                1,
                TurnItemPayload::UserMessage {
                    text: "stage 1".to_string(),
                },
            ),
            item(
                turn_a,
                2,
                TurnItemPayload::AgentMessage {
                    text: "stage 1 done".to_string(),
                },
            ),
            item(
                turn_a,
                3,
                TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            ),
            item(
                turn_b,
                1,
                TurnItemPayload::UserMessage {
                    text: "stage 2".to_string(),
                },
            ),
            item(
                turn_b,
                2,
                TurnItemPayload::AgentMessage {
                    text: "stage 2 done".to_string(),
                },
            ),
            item(
                turn_b,
                3,
                TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            ),
        ];
        let elapsed = std::collections::HashMap::from([(turn_a, 50_970), (turn_b, 91_889)]);
        let initial = build_session_detail(
            &canonical_read_with_elapsed(&session, initial_items.clone(), elapsed.clone()),
            None,
        );
        let initial_titles = initial
            .transcript_rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
            .map(|row| row.title.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            initial_titles,
            vec!["50s作業しました", "1m 31s作業しました"]
        );

        let mut later_items = initial_items;
        later_items.extend([
            item(
                turn_c,
                1,
                TurnItemPayload::UserMessage {
                    text: "stage 3".to_string(),
                },
            ),
            item(
                turn_c,
                2,
                TurnItemPayload::ContextCompaction {
                    summary: "later-turn compaction".to_string(),
                },
            ),
            item(
                turn_c,
                3,
                TurnItemPayload::AgentMessage {
                    text: "stage 3 done".to_string(),
                },
            ),
            item(
                turn_c,
                4,
                TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            ),
        ]);
        let mut later_elapsed = elapsed;
        later_elapsed.insert(turn_c, 125_000);
        let later = build_session_detail(
            &canonical_read_with_elapsed(&session, later_items, later_elapsed),
            None,
        );
        let later_titles = later
            .transcript_rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
            .map(|row| row.title.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            later_titles,
            vec!["50s作業しました", "1m 31s作業しました", "2m 5s作業しました",]
        );
    }

    #[test]
    fn completed_turn_item_transcript_folds_intermediate_assistant_control_feedback() {
        let session = session_record(ProjectId::new(), "workflow");
        let turn_id = crate::protocol::TurnId::new();
        let turn_items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::AgentMessage {
                    text: "Turn control projection surface: prompt\nInvalid tool arguments: context mismatch".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "verify-contract --behavior".to_string(),
                    summary: "Command: verify-contract --behavior\n\nStdout:\nOK".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 4,
                payload: TurnItemPayload::FileChange {
                    call_id: crate::session::ToolCallId::new(),
                    change_ids: vec![crate::session::ChangeId::new()],
                    changes: vec![FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    }],
                    summary: "Added src/workflow.rs".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 5,
                payload: TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 6,
                payload: TurnItemPayload::AgentMessage {
                    text:
                        "完了しました。src/workflow.rs を作成し、verify-contract --behavior は成功しました。"
                            .to_string(),
                },
            },
        ];

        let rows = transcript_rows_from_turn_items_with_context(&session, &turn_items);
        let assistant_rows = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .collect::<Vec<_>>();
        let primary_text = rows
            .iter()
            .filter(|row| row.row_kind != DesktopTranscriptRowKind::WorkSummaryCompleted)
            .map(|row| row.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let work_summary = rows
            .iter()
            .find(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
            .expect("work summary row");

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("完了しました"));
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::FileChanges)
        );
        assert!(!primary_text.contains("Turn control projection surface"));
        assert!(!primary_text.contains("Invalid tool arguments"));
        assert!(work_summary.body.contains("中間応答"));
    }

    #[test]
    fn multi_agent_turn_projects_typed_events_before_history_and_keeps_root_final_answer() {
        let session = session_record(ProjectId::new(), "multi-agent workflow");
        let turn_id = crate::protocol::TurnId::new();
        let child_session_id = SessionId::new();
        let turn_items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::UserMessage {
                    text: "review with a Sub Agent".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: TurnItemPayload::SubAgentActivity {
                    activity_id: "spawn-reviewer".to_string(),
                    agent_session_id: child_session_id,
                    agent_path: "/root/reviewer".to_string(),
                    activity_kind: crate::protocol::SubAgentActivityKind::Started,
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 3,
                payload: TurnItemPayload::InterAgentCommunication {
                    communication: crate::protocol::InterAgentCommunication {
                        author: "/root/reviewer".to_string(),
                        recipient: "/root".to_string(),
                        content: "**raw child report**".to_string(),
                        trigger_turn: false,
                    },
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 4,
                payload: TurnItemPayload::SubAgentActivity {
                    activity_id: "message-reviewer".to_string(),
                    agent_session_id: child_session_id,
                    agent_path: "/root/reviewer".to_string(),
                    activity_kind: crate::protocol::SubAgentActivityKind::Interacted,
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 5,
                payload: TurnItemPayload::AgentMessage {
                    text: "Root final summary names reviewer.".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 6,
                payload: TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            },
        ];

        let rows = transcript_rows_from_turn_items_with_context(&session, &turn_items);
        let row_index = |kind: DesktopTranscriptRowKind| {
            rows.iter()
                .position(|row| row.row_kind == kind)
                .expect("expected typed transcript row")
        };
        let started = row_index(DesktopTranscriptRowKind::SubAgentStarted);
        let updated = row_index(DesktopTranscriptRowKind::SubAgentUpdated);
        let work_summary = row_index(DesktopTranscriptRowKind::WorkSummaryCompleted);
        let final_answer = row_index(DesktopTranscriptRowKind::Assistant);

        assert!(row_index(DesktopTranscriptRowKind::User) < started);
        assert!(started < updated && updated < work_summary);
        assert!(work_summary < final_answer);
        assert_eq!(rows[started].title, "/root/reviewer");
        assert_eq!(rows[updated].title, "/root/reviewer");
        assert!(rows[updated].body.is_empty());
        assert_eq!(
            rows.iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::SubAgentUpdated)
                .count(),
            1,
            "one durable send_message interaction projects one Sub Agent update marker",
        );
        assert!(!rows.iter().any(|row| row.body.contains("raw child report")));
        assert!(rows[final_answer].body.contains("Root final summary"));
        assert!(!rows.iter().any(|row| {
            row.row_kind == DesktopTranscriptRowKind::System && row.title.contains("Sub Agent")
        }));
    }

    #[test]
    fn child_execution_projects_parent_followup_as_readable_instruction_not_root_agent_marker() {
        let session = session_record(ProjectId::new(), "child execution");
        let turn_id = crate::protocol::TurnId::new();
        let rows = transcript_rows_from_turn_items_with_context(
            &session,
            &[TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: TurnItemPayload::InterAgentCommunication {
                    communication: crate::protocol::InterAgentCommunication {
                        author: "/root".to_string(),
                        recipient: "/root/reviewer".to_string(),
                        content: "Please verify the final interaction.".to_string(),
                        trigger_turn: true,
                    },
                },
            }],
        );

        assert!(rows.iter().any(|row| {
            row.row_kind == DesktopTranscriptRowKind::System
                && row.title == "Agent間の追加指示"
                && row.body == "Please verify the final interaction."
        }));
        assert!(!rows.iter().any(|row| {
            row.row_kind == DesktopTranscriptRowKind::SubAgentUpdated && row.title == "/root"
        }));
    }

    #[test]
    fn completed_work_transcript_keeps_final_answer_and_folds_intermediate_assistant_prose() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 2, 1));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "まず tests/workflow.contract を作成します。".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "次に src/workflow.rs を作成します。".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body:
                    "完了しました。src/workflow.rs と tests/workflow.contract を作成し、検証も通りました。"
                        .to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        let rows = transcript_rows(&state);
        let assistant_rows = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("完了しました"));
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.body.contains("まず tests/workflow.contract"))
        );
    }

    #[test]
    fn completed_work_transcript_preserves_pseudo_tool_call_closeout_body() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 2, 1));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "検証は成功しました。\n<tool_call>\n<function=shell>\n<parameter=command>\nverify-contract --behavior\n</parameter>\n</function>\n</tool_call>".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        let rows = transcript_rows(&state);
        let assistant_rows = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("<tool_call>"));
        assert!(assistant_rows[0].body.contains("<parameter=command>"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
        );
    }

    #[test]
    fn completed_work_transcript_folds_intermediate_assistant_rows() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 3, 2));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "作業中です。verification evidence を確認しています。".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "完了しました。src/workflow.rs と tests/workflow.contract を作成しました。"
                    .to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        let rows = transcript_rows(&state);
        let assistant_rows = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("src/workflow.rs"));
        assert!(
            !rows
                .iter()
                .any(|row| row.body.contains("verification evidence を確認"))
        );
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
        );
    }

    #[test]
    fn completed_work_transcript_keeps_the_final_answer_for_each_user_turn() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 1, 0));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "first request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "first intermediate update".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "first final answer".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "second request".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "second final answer".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: String::new(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let rows = transcript_rows(&state);
        let assistant_bodies = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .map(|row| row.body.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            assistant_bodies,
            ["first final answer", "second final answer"]
        );
        let row_index = |body: &str| {
            rows.iter()
                .position(|row| row.body == body)
                .expect("expected transcript row")
        };
        let work_summary_index = rows
            .iter()
            .position(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
            .expect("expected work summary row");
        assert!(row_index("first request") < row_index("first final answer"));
        assert!(row_index("first final answer") < row_index("second request"));
        assert!(row_index("second request") < work_summary_index);
        assert!(work_summary_index < row_index("second final answer"));
        assert!(
            !rows
                .iter()
                .any(|row| row.body.contains("first intermediate update"))
        );
    }

    #[test]
    fn completed_work_transcript_preserves_closing_tag_only_pseudo_tool_call_fragment() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 3, 2));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "workflow_state.ready = true\n</parameter> <parameter=path> src/workflow.rs </parameter> </function> </tool_call>"
                    .to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        let rows = transcript_rows(&state);
        let assistant_rows = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("</tool_call>"));
        assert!(assistant_rows[0].body.contains("<parameter=path>"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
        );
    }

    #[test]
    fn completed_work_transcript_preserves_html_escaped_pseudo_tool_call_fragment() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 3, 2));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "workflow_state.ready = true\n&lt;/parameter&gt; &lt;parameter=path&gt; src/workflow.rs &lt;/parameter&gt; &lt;/function&gt; &lt;/tool_call&gt;"
                    .to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        let rows = transcript_rows(&state);
        let assistant_rows = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("&lt;/tool_call"));
        assert!(assistant_rows[0].body.contains("&lt;parameter=path"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryCompleted)
        );
    }

    #[test]
    fn idle_live_projection_preserves_pseudo_tool_call_closeout_evidence() {
        let project_id = ProjectId::new();
        let session = session_record(project_id, "workflow");
        let mut state = AppState::default();
        state.run_status = RunStatus::Idle;
        state.last_summary = Some(completed_run_summary_fixture(session.id, 3, 2));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "workflow_state.ready = true\n&lt;/parameter&gt; &lt;parameter=path&gt; src/workflow.rs &lt;/parameter&gt; &lt;/function&gt; &lt;/tool_call&gt;"
                    .to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "verify-contract --behavior".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];

        let rows = transcript_rows_with_context(&state, Some(&session), &[]);
        let assistant_rows = rows
            .iter()
            .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("&lt;/tool_call"));
        assert!(assistant_rows[0].body.contains("&lt;parameter=path"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryIncomplete)
        );
    }

    #[test]
    fn idle_live_projection_folds_intermediate_assistant_rows_without_cleanup() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Idle;
        state.last_summary = Some(completed_run_summary_fixture(SessionId::new(), 3, 2));
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "テスト失敗を修正します。".to_string(),
                response_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "完了しました。".to_string(),
                response_id: None,
                tool_call_id: None,
            },
        ];

        let rows = transcript_rows_with_context(&state, None, &[]);

        assert_eq!(
            rows.iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
                .count(),
            1
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.body.contains("テスト失敗を修正します"))
        );
        assert!(
            rows.iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::WorkSummaryIncomplete)
        );
    }
}
