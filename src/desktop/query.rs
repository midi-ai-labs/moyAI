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
    ChangeKind, ProjectId, ProjectRecord, SessionId, SessionRecord, SessionStateSnapshot,
    SessionStatus, TodoItem, TodoStatus, ToolCallStatus, Transcript,
};
use crate::tui::query::recent_sessions;
use crate::tui::state::{AppState, RunStatus, TranscriptKind};

use super::artifact_projection::{
    artifact_rows_from_file_changes, file_change_rows_from_turn_items_with_root,
    format_file_change_summary,
};
pub use super::artifact_projection::{file_change_rows_from_turn_items, format_artifact_preview};

pub const DESKTOP_TURN_PAGE_LIMIT: usize = 80;

pub async fn load_snapshot(app: &App, args: &DesktopArgs) -> Result<DesktopSnapshot, AppRunError> {
    load_snapshot_for_selection(app, args.session_id).await
}

pub async fn load_snapshot_for_selection(
    app: &App,
    selected_session_id: Option<SessionId>,
) -> Result<DesktopSnapshot, AppRunError> {
    let sessions = recent_sessions(&app.session_service, app.workspace.project_id, 20).await?;
    let selected_session_index = select_session_index(
        &sessions,
        selected_session_id,
        Some(app.workspace.project_id),
        false,
    )?;
    build_snapshot(app, sessions, selected_session_index).await
}

pub async fn load_snapshot_continue_last(app: &App) -> Result<DesktopSnapshot, AppRunError> {
    let sessions = recent_sessions(&app.session_service, app.workspace.project_id, 20).await?;
    let selected_session_index =
        select_session_index(&sessions, None, Some(app.workspace.project_id), true)?;
    build_snapshot(app, sessions, selected_session_index).await
}

pub async fn load_snapshot_for_session_search(
    app: &App,
    query: &str,
    include_archived: bool,
    selected_session_id: Option<SessionId>,
) -> Result<DesktopSnapshot, AppRunError> {
    let query = query.trim();
    if query.is_empty() {
        let sessions = app
            .session_service
            .list_sessions_with_archived(app.workspace.project_id, 50, include_archived)
            .await?;
        let selected_session_index = select_session_index(
            &sessions,
            selected_session_id,
            Some(app.workspace.project_id),
            false,
        )
        .unwrap_or(0);
        return build_snapshot(app, sessions, selected_session_index).await;
    }
    let sessions = app
        .session_service
        .search_sessions(app.workspace.project_id, query, 50, include_archived)
        .await?;
    let selected_session_index = select_session_index(
        &sessions,
        selected_session_id,
        Some(app.workspace.project_id),
        false,
    )
    .unwrap_or(0);
    build_snapshot(app, sessions, selected_session_index).await
}

pub async fn load_session_detail(
    app: &App,
    session_id: SessionId,
) -> Result<
    (
        SessionRecord,
        Transcript,
        Vec<crate::protocol::TurnItem>,
        SessionStateSnapshot,
        Vec<TodoItem>,
        usize,
        usize,
        usize,
        bool,
    ),
    AppRunError,
> {
    let session = app.session_service.get_session(session_id).await?;
    let page = app
        .session_service
        .canonical_turn_page(session_id, 0, DESKTOP_TURN_PAGE_LIMIT)
        .await?;
    let transcript = app.session_service.canonical_transcript(session_id).await?;
    let state = app.session_service.load_state(session_id).await?;
    let todos = app.session_service.list_todos(session_id).await?;
    Ok((
        session,
        transcript,
        page.items,
        state,
        todos,
        page.offset,
        page.limit,
        page.total,
        page.has_more,
    ))
}

async fn build_snapshot(
    app: &App,
    sessions: Vec<SessionRecord>,
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
    for session in &sessions {
        session_rows.push(build_session_row(app, session).await?);
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
    let sessions = recent_sessions(&app.session_service, project_id, 20).await?;
    let mut rows = Vec::with_capacity(sessions.len());
    for session in &sessions {
        rows.push(build_session_row(app, session).await?);
    }
    Ok(rows)
}

async fn build_session_row(
    app: &App,
    session: &SessionRecord,
) -> Result<DesktopSessionRow, AppRunError> {
    let summary = app
        .session_service
        .loaded_session_summary(session.clone())
        .await?;
    Ok(DesktopSessionRow::from_loaded_summary(&summary))
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
    session: &SessionRecord,
    state: SessionStateSnapshot,
    todos: Vec<TodoItem>,
    _transcript: Transcript,
    turn_items: Vec<crate::protocol::TurnItem>,
    replay_report: Option<ReplayReport>,
) -> DesktopSessionDetail {
    build_session_detail_with_page(
        session,
        state,
        todos,
        _transcript,
        turn_items,
        0,
        0,
        0,
        false,
        replay_report,
    )
}

pub fn build_session_detail_with_page(
    session: &SessionRecord,
    state: SessionStateSnapshot,
    todos: Vec<TodoItem>,
    _transcript: Transcript,
    turn_items: Vec<crate::protocol::TurnItem>,
    turn_page_offset: usize,
    turn_page_limit: usize,
    turn_page_total: usize,
    turn_page_has_more: bool,
    replay_report: Option<ReplayReport>,
) -> DesktopSessionDetail {
    let mut ui_state = AppState::default();
    ui_state.load_turn_items(session, &turn_items, state.clone(), todos.clone());
    let file_changes =
        file_change_rows_from_turn_items_with_root(&turn_items, Some(session.cwd.as_path()));
    let mut detail = build_session_detail_from_app_state(&ui_state);
    detail.turn_page_offset = turn_page_offset;
    detail.turn_page_limit = if turn_page_limit == 0 {
        turn_items.len()
    } else {
        turn_page_limit
    };
    detail.turn_page_total = if turn_page_total == 0 {
        turn_items.len()
    } else {
        turn_page_total
    };
    detail.turn_page_has_more = turn_page_has_more;
    detail.session_id = session.id;
    detail.transcript_rows = transcript_rows_from_turn_items_with_context(session, &turn_items);
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
    user_body: String,
    assistant_bodies: Vec<String>,
    tool_rows: Vec<String>,
    file_change_items: Vec<crate::protocol::TurnItem>,
    system_rows: Vec<DesktopTranscriptRow>,
    terminal_summary: Option<String>,
    terminal_status: Option<crate::protocol::TurnTerminalStatus>,
}

impl TurnTranscriptGroup {
    fn has_content(&self) -> bool {
        !self.user_body.trim().is_empty()
            || !self.assistant_bodies.is_empty()
            || !self.tool_rows.is_empty()
            || !self.file_change_items.is_empty()
            || !self.system_rows.is_empty()
            || self.terminal_summary.is_some()
    }
}

fn transcript_rows_from_turn_items_with_context(
    session: &SessionRecord,
    turn_items: &[crate::protocol::TurnItem],
) -> Vec<DesktopTranscriptRow> {
    let mut rows = Vec::new();
    let mut current = TurnTranscriptGroup::default();
    let ordered = ordered_turn_items_for_projection(turn_items);
    let show_session_elapsed_on_work_summary = ordered
        .iter()
        .filter(|item| {
            matches!(
                item.payload,
                crate::protocol::TurnItemPayload::UserMessage { .. }
                    | crate::protocol::TurnItemPayload::SteerMessage { .. }
            )
        })
        .count()
        <= 1;

    for item in ordered {
        match &item.payload {
            crate::protocol::TurnItemPayload::UserMessage { text } => {
                flush_turn_transcript_group(
                    &mut rows,
                    session,
                    &mut current,
                    show_session_elapsed_on_work_summary,
                );
                current.user_body = text.clone();
            }
            crate::protocol::TurnItemPayload::SteerMessage { text } => {
                flush_turn_transcript_group(
                    &mut rows,
                    session,
                    &mut current,
                    show_session_elapsed_on_work_summary,
                );
                current.user_body = text.clone();
            }
            crate::protocol::TurnItemPayload::AgentMessage { text } => {
                current.assistant_bodies.push(text.clone());
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
            crate::protocol::TurnItemPayload::Terminal { status, summary } => {
                current.terminal_summary = Some(summary.clone());
                current.terminal_status = Some(*status);
            }
            crate::protocol::TurnItemPayload::Reasoning { .. }
            | crate::protocol::TurnItemPayload::Plan { .. }
            | crate::protocol::TurnItemPayload::PromptDispatch { .. }
            | crate::protocol::TurnItemPayload::State { .. }
            | crate::protocol::TurnItemPayload::LifecycleGuard { .. } => {}
        }
    }
    flush_turn_transcript_group(
        &mut rows,
        session,
        &mut current,
        show_session_elapsed_on_work_summary,
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
        kind: row_kind.key().to_string(),
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
    show_session_elapsed_on_work_summary: bool,
) {
    if !group.has_content() {
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
    rows.extend(group.system_rows.drain(..));

    let file_changes = file_change_rows_from_turn_items_with_root(
        &group.file_change_items,
        Some(session.cwd.as_path()),
    );
    let has_work_summary = turn_group_has_work_summary(group, &file_changes);
    if has_work_summary {
        let row_kind = turn_work_summary_kind(group);
        rows.push(desktop_transcript_row(
            row_kind,
            String::new(),
            if show_session_elapsed_on_work_summary {
                session_elapsed_label(session)
                    .map(|value| format!("{value}作業しました"))
                    .unwrap_or_else(|| "作業履歴 / 作業サマリ".to_string())
            } else {
                "作業履歴 / 作業サマリ".to_string()
            },
            turn_work_summary_body(group, &file_changes),
            Vec::new(),
        ));
    }
    for body in primary_assistant_bodies_for_turn_group(group, &file_changes) {
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
    group.terminal_summary = None;
    group.terminal_status = None;
}

fn turn_work_summary_kind(group: &TurnTranscriptGroup) -> DesktopTranscriptRowKind {
    match group.terminal_status {
        Some(crate::protocol::TurnTerminalStatus::Failed) => {
            DesktopTranscriptRowKind::WorkSummaryFailed
        }
        Some(crate::protocol::TurnTerminalStatus::Interrupted) => {
            DesktopTranscriptRowKind::WorkSummaryCancelled
        }
        Some(crate::protocol::TurnTerminalStatus::AwaitingUser) => {
            DesktopTranscriptRowKind::WorkSummaryAwaitingUser
        }
        Some(crate::protocol::TurnTerminalStatus::Completed) | None => {
            DesktopTranscriptRowKind::WorkSummaryCompleted
        }
    }
}

fn turn_group_has_work_summary(
    group: &TurnTranscriptGroup,
    file_changes: &[DesktopFileChangeRow],
) -> bool {
    !group.tool_rows.is_empty() || !file_changes.is_empty() || group.terminal_summary.is_some()
}

fn primary_assistant_bodies_for_turn_group(
    group: &TurnTranscriptGroup,
    file_changes: &[DesktopFileChangeRow],
) -> Vec<String> {
    let bodies = group
        .assistant_bodies
        .iter()
        .map(|body| body.trim())
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>();
    if bodies.len() <= 1 || !turn_group_has_work_summary(group, file_changes) {
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
) -> String {
    let mut sections = Vec::new();
    sections.push(format!(
        "### 作業サマリ\n{}",
        completed_turn_summary_text(group, file_changes)
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

fn completed_turn_summary_text(
    group: &TurnTranscriptGroup,
    file_changes: &[DesktopFileChangeRow],
) -> String {
    let status = group
        .terminal_summary
        .as_ref()
        .map(|summary| terminal_summary_label(summary))
        .unwrap_or_else(|| "作業履歴を記録しました。".to_string());
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
        "session completed" => "セッションは完了しました。".to_string(),
        "session awaiting user" => "ユーザー確認待ちで停止しました。".to_string(),
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
        crate::protocol::ToolLifecycleStatus::Pending
        | crate::protocol::ToolLifecycleStatus::Blocked
        | crate::protocol::ToolLifecycleStatus::Rejected
        | crate::protocol::ToolLifecycleStatus::Deferred => "待機",
        crate::protocol::ToolLifecycleStatus::Running => "実行中",
        crate::protocol::ToolLifecycleStatus::Completed => "完了",
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
    let session_state = state.session_state.clone().unwrap_or_default();
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
        tool_status_text: format_tool_status_text(state, &session_state, &state.sidebar_todos),
        progress_text: format_progress_text(state),
        run_status_text: format_run_status_text(state, &session_state),
        confirmation_text: format_confirmation_text(state),
        confirmation_visible: state.permission.is_some(),
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
            .map(|summary| session_status_is_terminal(summary.status))
            .unwrap_or(false)
        || session
            .map(|session| session_status_is_terminal(session.status))
            .unwrap_or(false);
    let work_summary = work_summary_row(state, session, file_changes);
    let mut rows = fold_intermediate_assistant_rows(
        base_rows,
        state,
        file_changes,
        work_summary.is_some(),
        terminal,
    );
    if let Some(work_summary) = work_summary {
        let insert_index = rows
            .iter()
            .position(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
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
            || !state.sidebar_todos.is_empty()
            || !file_changes.is_empty()
            || state.last_summary.is_some());
    if !should_fold {
        return rows;
    }
    let last_assistant_index = rows.iter().rposition(|row| {
        row.row_kind == DesktopTranscriptRowKind::Assistant && !row.body.trim().is_empty()
    });
    let assistant_count = rows
        .iter()
        .filter(|row| row.row_kind == DesktopTranscriptRowKind::Assistant)
        .count();
    if assistant_count <= 1 {
        return rows;
    }
    rows.into_iter()
        .enumerate()
        .filter_map(|(index, row)| {
            if row.row_kind == DesktopTranscriptRowKind::Assistant
                && Some(index) != last_assistant_index
            {
                None
            } else {
                Some(row)
            }
        })
        .collect()
}

const CURRENT_PROVIDER_PROFILE_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";
const CURRENT_PROVIDER_PROFILE_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";

fn desktop_query_provider_profile_session_record(title: &str) -> SessionRecord {
    SessionRecord {
        id: SessionId::new(),
        project_id: ProjectId::new(),
        title: title.to_string(),
        status: SessionStatus::Completed,
        cwd: camino::Utf8PathBuf::from(format!("C:/workspace/{title}")),
        model: CURRENT_PROVIDER_PROFILE_FIXTURE_MODEL.to_string(),
        base_url: CURRENT_PROVIDER_PROFILE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1_000,
        updated_at_ms: 6_000,
        completed_at_ms: Some(6_000),
    }
}

pub(crate) fn desktop_query_current_provider_profile_fixture_passes() -> bool {
    let session = desktop_query_provider_profile_session_record("desktop-query-provider-fixture");
    session.base_url == CURRENT_PROVIDER_PROFILE_FIXTURE_BASE_URL
        && session.model == CURRENT_PROVIDER_PROFILE_FIXTURE_MODEL
        && completed_desktop_transcript_primary_reading_fixture_passes()
        && desktop_turn_item_projection_uses_turn_local_sequence_fixture_passes()
}

pub fn completed_desktop_transcript_primary_reading_fixture_passes() -> bool {
    let fixture_language_neutral_ref = "desktop_transcript_fixture_language_neutral";
    let session = desktop_query_provider_profile_session_record("desktop-transcript-fixture");
    let turn_id = crate::protocol::TurnId::new();
    let rows = transcript_rows_from_turn_items_with_context(
        &session,
        &[
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: crate::protocol::TurnItemPayload::UserMessage {
                    text: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: crate::protocol::TurnItemPayload::AgentMessage {
                    text: "Turn control projection surface: prompt\nInvalid tool arguments: context mismatch".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 3,
                payload: crate::protocol::TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "verify-contract --behavior".to_string(),
                    summary: "Command: verify-contract --behavior\n\nStdout:\nOK".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 4,
                payload: crate::protocol::TurnItemPayload::FileChange {
                    call_id: crate::session::ToolCallId::new(),
                    change_ids: vec![crate::session::ChangeId::new()],
                    changes: vec![crate::protocol::FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(camino::Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    }],
                    summary: "Added src/workflow.rs".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 5,
                payload: crate::protocol::TurnItemPayload::Terminal {
                    status: crate::protocol::TurnTerminalStatus::Completed,
                    summary: "session completed".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 6,
                payload: crate::protocol::TurnItemPayload::AgentMessage {
                    text: "完了しました。src/workflow.rs を作成し、verify-contract --behavior は成功しました。".to_string(),
                },
            },
        ],
    );

    let assistant_rows = rows
        .iter()
        .filter(|row| row.kind == "assistant")
        .collect::<Vec<_>>();
    assistant_rows.len() == 1
        && assistant_rows[0].body.contains("完了しました")
        && rows.iter().any(|row| row.kind == "work_summary_completed")
        && rows.iter().any(|row| row.kind == "file_changes")
        && fixture_language_neutral_ref == "desktop_transcript_fixture_language_neutral"
        && !rows.iter().any(|row| {
            row.kind == "assistant"
                && (row.body.contains("Turn control projection surface")
                    || row.body.contains("Invalid tool arguments")
                    || row.body.contains("context mismatch"))
        })
}

pub fn desktop_pseudo_tool_call_closeout_evidence_preserved_fixture_passes() -> bool {
    let mut state = AppState::default();
    state.run_status = RunStatus::Completed;
    state.last_summary = Some(crate::session::RunSummary {
        session_id: SessionId::new(),
        assistant_message_id: None,
        status: SessionStatus::Completed,
        finish_reason: None,
        tool_call_count: 1,
        failed_tool_count: 0,
        change_count: 1,
    });
    state.transcript_entries = vec![
        crate::tui::state::TranscriptEntry {
            kind: TranscriptKind::User,
            title: "User".to_string(),
            body: "workflow artifact を作成".to_string(),
            message_id: None,
            tool_call_id: None,
        },
        crate::tui::state::TranscriptEntry {
            kind: TranscriptKind::Assistant,
            title: "Assistant".to_string(),
            body: "検証済みです。\n<tool_call>\n<function=shell>\n<parameter=command>\nverify-contract --behavior\n</parameter>\n</function>\n</tool_call>"
                .to_string(),
            message_id: None,
            tool_call_id: None,
        },
    ];

    let rows = transcript_rows_with_context(&state, None, &[]);
    let assistant_rows = rows
        .iter()
        .filter(|row| row.kind == "assistant")
        .collect::<Vec<_>>();

    assistant_rows.len() == 1
        && assistant_rows[0].body.contains("<tool_call>")
        && assistant_rows[0].body.contains("<parameter=command>")
        && assistant_rows[0].body != "完了しました。"
        && rows.iter().any(|row| row.kind == "work_summary_completed")
}

pub(crate) fn desktop_turn_item_projection_uses_turn_local_sequence_fixture_passes() -> bool {
    let mut session = desktop_query_provider_profile_session_record("desktop-turn-order-fixture");
    session.updated_at_ms = 2_000;
    session.completed_at_ms = Some(2_000);
    let turn_id = crate::protocol::TurnId::new();
    let rows = transcript_rows_from_turn_items_with_context(
        &session,
        &[
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 3,
                payload: crate::protocol::TurnItemPayload::FileChange {
                    call_id: crate::session::ToolCallId::new(),
                    change_ids: vec![crate::session::ChangeId::new()],
                    changes: vec![crate::protocol::FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(camino::Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    }],
                    summary: "Added src/workflow.rs".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: crate::protocol::TurnItemPayload::UserMessage {
                    text: "src/workflow.rs を作成".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 4,
                payload: crate::protocol::TurnItemPayload::AgentMessage {
                    text: "完了しました。".to_string(),
                },
            },
            crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: crate::protocol::TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Write,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "write src/workflow.rs".to_string(),
                    summary: "wrote src/workflow.rs".to_string(),
                },
            },
        ],
    );
    let user = rows
        .iter()
        .position(|row| row.kind == "user" && row.body.contains("src/workflow.rs"));
    let summary = rows
        .iter()
        .position(|row| row.kind == "work_summary_completed");
    let assistant = rows
        .iter()
        .position(|row| row.kind == "assistant" && row.body.contains("完了"));
    let file_changes = rows.iter().position(|row| row.kind == "file_changes");
    matches!(
        (user, summary, assistant, file_changes),
        (Some(user), Some(summary), Some(assistant), Some(file_changes))
            if user < summary && summary < assistant && assistant < file_changes
    )
}

pub(crate) fn desktop_query_todo_status_typed_projection_fixture_passes() -> bool {
    let todos = vec![
        TodoItem::simple(
            "workflow source ready",
            TodoStatus::Completed,
            crate::session::TodoPriority::High,
        ),
        TodoItem::simple(
            "workflow verification running",
            TodoStatus::InProgress,
            crate::session::TodoPriority::High,
        ),
        TodoItem::simple(
            "workflow release blocked",
            TodoStatus::Blocked,
            crate::session::TodoPriority::Medium,
        ),
        TodoItem::simple(
            "workflow follow-up queued",
            TodoStatus::Pending,
            crate::session::TodoPriority::Low,
        ),
        TodoItem::simple(
            "workflow obsolete branch",
            TodoStatus::Cancelled,
            crate::session::TodoPriority::Low,
        ),
    ];
    let title = task_summary_title(&todos);
    let rows = format_todo_rows(&todos);
    let mut state = AppState::default();
    state.sidebar_todos = todos.clone();
    let tool_status_text =
        format_tool_status_text(&state, &SessionStateSnapshot::default(), &todos);

    title == "タスク進捗 1/5 完了, 1件ブロック"
        && rows.contains("✓ workflow source ready  (状態: 完了")
        && rows.contains("● workflow verification running  (状態: 進行中")
        && rows.contains("! workflow release blocked  (状態: ブロック")
        && rows.contains("○ workflow follow-up queued  (状態: 未着手")
        && rows.contains("○ workflow obsolete branch  (状態: 停止")
        && tool_status_text.contains("- [完了] workflow source ready")
        && tool_status_text.contains("- [進行中] workflow verification running")
        && tool_status_text.contains("- [ブロック] workflow release blocked")
        && tool_status_text.contains("- [停止] workflow obsolete branch")
}

pub(crate) fn desktop_transcript_row_kind_typed_projection_fixture_passes() -> bool {
    let mut state = AppState::default();
    state.run_status = RunStatus::Completed;
    let mut intermediate = desktop_transcript_row(
        DesktopTranscriptRowKind::Assistant,
        "01".to_string(),
        "応答".to_string(),
        "中間応答".to_string(),
        Vec::new(),
    );
    let mut final_answer = desktop_transcript_row(
        DesktopTranscriptRowKind::Assistant,
        "02".to_string(),
        "応答".to_string(),
        "完了しました。".to_string(),
        Vec::new(),
    );
    intermediate.kind = DesktopTranscriptRowKind::System.key().to_string();
    final_answer.kind = DesktopTranscriptRowKind::System.key().to_string();
    let file_changes = vec![DesktopFileChangeRow {
        label: "workflow.rs".to_string(),
        path: "src/workflow.rs".to_string(),
        kind: ChangeKind::Add,
        action: "追加".to_string(),
        summary: "Added src/workflow.rs".to_string(),
        tool_call_ids: vec![crate::session::ToolCallId::new()],
    }];

    let folded = fold_intermediate_assistant_rows(
        vec![intermediate, final_answer],
        &state,
        &file_changes,
        true,
        true,
    );

    folded.len() == 1
        && folded[0].row_kind == DesktopTranscriptRowKind::Assistant
        && folded[0].kind == DesktopTranscriptRowKind::System.key()
        && folded[0].body.contains("完了しました")
        && DesktopTranscriptRowKind::FileChanges.key() == "file_changes"
}

fn session_status_is_terminal(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Completed
            | SessionStatus::AwaitingUser
            | SessionStatus::Cancelled
            | SessionStatus::Failed
    )
}

fn is_internal_transcript_projection(kind: DesktopTranscriptRowKind, title: &str) -> bool {
    matches!(
        kind,
        DesktopTranscriptRowKind::Tool
            | DesktopTranscriptRowKind::Summary
            | DesktopTranscriptRowKind::Diff
            | DesktopTranscriptRowKind::Reasoning
            | DesktopTranscriptRowKind::Editing
    ) || matches!(kind, DesktopTranscriptRowKind::System)
        && !title.eq_ignore_ascii_case("User")
        && !title.eq_ignore_ascii_case("Context Compaction")
}

fn work_summary_row(
    state: &AppState,
    session: Option<&SessionRecord>,
    file_changes: &[DesktopFileChangeRow],
) -> Option<DesktopTranscriptRow> {
    let has_work = !state.tool_statuses.is_empty()
        || !state.sidebar_todos.is_empty()
        || !file_changes.is_empty()
        || state.last_summary.is_some()
        || matches!(state.run_status, RunStatus::Running | RunStatus::Confirming);
    if !has_work {
        return None;
    }

    let kind = match state.run_status {
        RunStatus::Running | RunStatus::Confirming => DesktopTranscriptRowKind::WorkSummaryRunning,
        RunStatus::Failed => DesktopTranscriptRowKind::WorkSummaryFailed,
        RunStatus::Cancelled => DesktopTranscriptRowKind::WorkSummaryCancelled,
        RunStatus::AwaitingUser => DesktopTranscriptRowKind::WorkSummaryAwaitingUser,
        _ => DesktopTranscriptRowKind::WorkSummaryCompleted,
    };
    Some(desktop_transcript_row(
        kind,
        String::new(),
        work_summary_title(state, session),
        work_summary_body(state, file_changes),
        Vec::new(),
    ))
}

fn work_summary_title(state: &AppState, session: Option<&SessionRecord>) -> String {
    let elapsed = session.and_then(session_elapsed_label);
    match state.run_status {
        RunStatus::Running => elapsed
            .map(|value| format!("{value} 作業中"))
            .unwrap_or_else(|| "作業中".to_string()),
        RunStatus::Confirming => elapsed
            .map(|value| format!("{value} 確認待ち"))
            .unwrap_or_else(|| "確認待ち".to_string()),
        RunStatus::Failed => elapsed
            .map(|value| format!("{value}で失敗しました"))
            .unwrap_or_else(|| "失敗しました".to_string()),
        RunStatus::Cancelled => elapsed
            .map(|value| format!("{value}で停止しました"))
            .unwrap_or_else(|| "停止しました".to_string()),
        _ => elapsed
            .map(|value| format!("{value}作業しました"))
            .unwrap_or_else(|| "作業しました".to_string()),
    }
}

fn session_elapsed_label(session: &SessionRecord) -> Option<String> {
    let end = session
        .completed_at_ms
        .unwrap_or(session.updated_at_ms)
        .max(session.created_at_ms);
    let elapsed_ms = end.checked_sub(session.created_at_ms)?;
    Some(format_duration(elapsed_ms))
}

fn format_duration(elapsed_ms: i64) -> String {
    let total_seconds = (elapsed_ms / 1000).max(0);
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
    if state.last_summary.is_some() || state.run_status.is_terminal() {
        sections.push(format!(
            "### 作業サマリ\n{}",
            current_run_summary_text(state, file_changes)
        ));
    }
    if matches!(state.run_status, RunStatus::Running | RunStatus::Confirming) {
        sections.push(format!(
            "### 現在\n- フェーズ: {}\n- 手順: {}\n- モデル要求: {}",
            state.progress.current_phase, state.progress.active_step, state.progress.model_requests
        ));
    }
    if !state.sidebar_todos.is_empty() {
        sections.push(format!(
            "### タスク\n{}\n{}",
            task_summary_title(&state.sidebar_todos),
            format_todo_rows(&state.sidebar_todos)
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
    if let Some(summary) = &state.last_summary {
        sections.push(format!(
            "### 完了\n- 状態: {}\n- ツール: {}件実行 / {}件失敗\n- ファイル変更: {}件",
            format_session_status(summary.status),
            summary.tool_call_count,
            summary.failed_tool_count,
            summary.change_count
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
        .map(|summary| format_session_status(summary.status).to_string())
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
        lines.push(format!(
            "- コマンド/ツール: {}件完了{}",
            completed,
            if failed > 0 {
                format!(" / {failed}件失敗")
            } else {
                String::new()
            }
        ));
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

fn task_summary_title(todos: &[TodoItem]) -> String {
    let completed = todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Completed))
        .count();
    let blocked = todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Blocked))
        .count();
    if blocked > 0 {
        format!(
            "タスク進捗 {completed}/{} 完了, {blocked}件ブロック",
            todos.len()
        )
    } else {
        format!("タスク進捗 {completed}/{} 完了", todos.len())
    }
}

fn format_todo_rows(todos: &[TodoItem]) -> String {
    todos
        .iter()
        .map(|todo| {
            let marker = todo_status_marker(todo.status);
            format!(
                "{marker} {}  (状態: {} / 種別: {:?})",
                todo.content,
                todo_status_label(todo.status),
                todo.kind
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    let running = tools.len().saturating_sub(completed + failed);
    if running > 0 {
        format!("{completed}件のコマンドを実行, {running}件実行中")
    } else if failed > 0 {
        format!("{completed}件のコマンドを実行, {failed}件失敗")
    } else {
        format!("{completed}件のコマンドを実行")
    }
}

fn transcript_row_kind_from_entry(kind: TranscriptKind) -> DesktopTranscriptRowKind {
    match kind {
        TranscriptKind::User => DesktopTranscriptRowKind::User,
        TranscriptKind::Assistant => DesktopTranscriptRowKind::Assistant,
        TranscriptKind::Reasoning => DesktopTranscriptRowKind::Reasoning,
        TranscriptKind::Editing => DesktopTranscriptRowKind::Editing,
        TranscriptKind::Tool => DesktopTranscriptRowKind::Tool,
        TranscriptKind::CommandSummary => DesktopTranscriptRowKind::Summary,
        TranscriptKind::Diff => DesktopTranscriptRowKind::Diff,
        TranscriptKind::System => DesktopTranscriptRowKind::System,
        TranscriptKind::Error => DesktopTranscriptRowKind::Error,
    }
}

fn format_tool_status_text(
    state: &AppState,
    session_state: &SessionStateSnapshot,
    todos: &[TodoItem],
) -> String {
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
    if !todos.is_empty() {
        lines.push(String::new());
        lines.push("タスク:".to_string());
        lines.extend(
            todos
                .iter()
                .map(|todo| format!("- [{}] {}", todo_status_label(todo.status), todo.content)),
        );
    }
    if let Some(summary) = &session_state.completion.route_contract_summary {
        lines.push(String::new());
        lines.push(format!("契約: {summary}"));
    }
    if let Some(handoff) = &session_state.implementation_handoff {
        if !handoff.next_actions.is_empty() {
            lines.push(String::new());
            lines.push("次の操作:".to_string());
            lines.extend(
                handoff
                    .next_actions
                    .iter()
                    .take(3)
                    .map(|value| format!("- {value}")),
            );
        }
    }
    if let Some(failure) = &session_state.failure {
        lines.push(String::new());
        lines.push(format!("失敗: {}", failure.summary));
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

fn format_run_status_text(state: &AppState, session_state: &SessionStateSnapshot) -> String {
    let mut lines = vec![run_status_label(state.run_status).to_string()];
    lines.push(format!("ルート: {:?}", session_state.route).to_lowercase());
    lines.push(format!("フェーズ: {:?}", session_state.process_phase).to_lowercase());
    if let Some(message) = &state.status_message {
        lines.push(format!("状態: {message}"));
    }
    lines.push(format!(
        "未完了作業: {}",
        session_state.completion.open_work_count
    ));
    if session_state.completion.verification_pending {
        lines.push("検証: 未実行".to_string());
    }
    if let Some(blocked) = &session_state.completion.blocked_reason {
        lines.push(format!("ブロック: {blocked}"));
    }
    lines.join("\n")
}

fn format_progress_text(state: &AppState) -> String {
    let progress = &state.progress;
    vec![
        progress.status.clone(),
        format!("フェーズ: {}", progress.current_phase),
        format!("手順: {}", progress.active_step),
        format!("モデル要求: {}", progress.model_requests),
        format!(
            "ツール: {}件開始 / {}件完了 / {}件失敗",
            progress.tool_calls_started, progress.tool_calls_completed, progress.tool_calls_failed
        ),
        format!("拒否した提案: {}", progress.rejected_tool_proposals),
        format!("圧縮: {}", progress.compactions),
        format!("再試行: {}", progress.retries),
    ]
    .join("\n")
}

fn format_confirmation_text(state: &AppState) -> String {
    let Some(permission) = &state.permission else {
        return String::new();
    };
    let targets = if permission.targets.is_empty() {
        "(なし)".to_string()
    } else {
        permission.targets.join(", ")
    };
    let risks = if permission.risks.is_empty() {
        "なし".to_string()
    } else {
        permission.risks.join(", ")
    };
    let details = if permission.details.is_empty() {
        "なし".to_string()
    } else {
        permission.details.join("\n")
    };
    format!(
        "{}\n\n実行内容:\n{details}\n\n対象: {targets}\nワークスペース外: {}\nリスク: {risks}",
        permission.summary,
        if permission.outside_workspace {
            "はい"
        } else {
            "いいえ"
        }
    )
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "待機中",
        RunStatus::Running => "実行中",
        RunStatus::Confirming => "確認待ち",
        RunStatus::Completed => "完了",
        RunStatus::AwaitingUser => "ユーザー確認待ち",
        RunStatus::Cancelled => "停止済み",
        RunStatus::Failed => "失敗",
    }
}

fn entry_heading(kind: TranscriptKind, title: &str) -> String {
    match kind {
        TranscriptKind::User => "ユーザー依頼".to_string(),
        TranscriptKind::Assistant => "応答".to_string(),
        TranscriptKind::Reasoning => "推論".to_string(),
        TranscriptKind::Editing => "編集中".to_string(),
        TranscriptKind::Tool => format!("コマンド / ツール - {title}"),
        TranscriptKind::CommandSummary => title.to_string(),
        TranscriptKind::Diff => format!("ファイル変更 - {title}"),
        TranscriptKind::System => format!("システム - {title}"),
        TranscriptKind::Error => format!("エラー - {title}"),
    }
}

fn todo_status_marker(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Completed => "✓",
        TodoStatus::InProgress => "●",
        TodoStatus::Blocked => "!",
        TodoStatus::Pending | TodoStatus::Cancelled => "○",
    }
}

fn todo_status_label(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Completed => "完了",
        TodoStatus::InProgress => "進行中",
        TodoStatus::Blocked => "ブロック",
        TodoStatus::Pending => "未着手",
        TodoStatus::Cancelled => "停止",
    }
}

fn tool_call_status_label(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "待機中",
        ToolCallStatus::Running => "実行中",
        ToolCallStatus::Completed => "完了",
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

    fn session_record(project_id: ProjectId, title: &str) -> SessionRecord {
        let mut session = desktop_query_provider_profile_session_record(title);
        session.project_id = project_id;
        session.created_at_ms = 1;
        session.updated_at_ms = 2;
        session.completed_at_ms = Some(2);
        session
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
            .position(|row| row.kind == "user" && row.body.contains("指示プロンプトA"))
            .expect("user A row");
        let index_change_a = rows
            .iter()
            .position(|row| {
                row.kind == "file_changes" && row.body.contains("docs/workflow-notes.md")
            })
            .expect("file change A row");
        let index_assistant_a = rows
            .iter()
            .position(|row| row.kind == "assistant" && row.body.contains("応答A"))
            .expect("assistant A row");
        let index_user_b = rows
            .iter()
            .position(|row| row.kind == "user" && row.body.contains("指示プロンプトB"))
            .expect("user B row");
        let index_change_b = rows
            .iter()
            .position(|row| {
                row.kind == "file_changes" && row.body.contains("tests/workflow.contract")
            })
            .expect("file change B row");
        let index_assistant_b = rows
            .iter()
            .position(|row| row.kind == "assistant" && row.body.contains("応答B"))
            .expect("assistant B row");

        assert!(index_user_a < index_assistant_a);
        assert!(index_assistant_a < index_change_a);
        assert!(index_user_a < index_change_a);
        assert!(index_change_a < index_user_b);
        assert!(index_user_b < index_assistant_b);
        assert!(index_assistant_b < index_change_b);
        assert!(index_user_b < index_change_b);
        assert_eq!(
            rows.iter().filter(|row| row.kind == "file_changes").count(),
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
                .filter(|row| row.kind.starts_with("work_summary"))
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
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Tool,
                title: "write".to_string(),
                body: "src/workflow.rs [Completed]".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Diff,
                title: "File changes".to_string(),
                body: "Added src/workflow.rs".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Context Compaction".to_string(),
                body: "圧縮しました\n\nCompactionContinuity".to_string(),
                message_id: None,
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
            .find(|row| row.kind == "work_summary_completed")
            .expect("tool status should be projected into a work summary");
        assert!(summary.body.contains("1件のコマンドを実行"));
        assert!(!rows.iter().any(|row| row.kind == "tool"));

        state.run_status = RunStatus::Running;
        state.progress.active_step = "Running verify-contract --behavior".to_string();
        state.progress.current_phase = "tool".to_string();
        state.sidebar_todos = vec![
            TodoItem::simple(
                "src/workflow.rsを作成",
                crate::session::TodoStatus::Completed,
                crate::session::TodoPriority::High,
            ),
            TodoItem::simple(
                "contract verificationを実行",
                crate::session::TodoStatus::InProgress,
                crate::session::TodoPriority::High,
            ),
        ];
        state.last_summary = Some(crate::session::RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 1,
            failed_tool_count: 0,
            change_count: 1,
        });

        let rows = transcript_rows(&state);
        let summary = rows
            .iter()
            .find(|row| row.kind == "work_summary_running")
            .expect("running state should be projected into an expanded work summary");
        assert_eq!(summary.title, "作業中");
        assert!(summary.body.contains("タスク進捗"));
        assert!(summary.body.contains("完了サマリ") || summary.body.contains("### 完了"));
        assert!(!rows.iter().any(|row| row.title == "完了サマリ"));
    }

    #[test]
    fn stored_session_work_summary_uses_elapsed_time_and_hides_internal_rows() {
        let project_id = ProjectId::new();
        let mut session = session_record(project_id, "workflow");
        session.created_at_ms = 1_000;
        session.updated_at_ms = 108_000;
        session.completed_at_ms = Some(108_000);
        let state = SessionStateSnapshot::default();
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
                    status: crate::protocol::TurnTerminalStatus::Completed,
                    summary: "session completed".to_string(),
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

        let detail = build_session_detail(
            &session,
            state,
            Vec::new(),
            Transcript {
                session: session.clone(),
                messages: Vec::new(),
            },
            turn_items,
            None,
        );

        assert!(
            detail.transcript_rows.iter().any(
                |row| row.kind == "work_summary_completed" && row.title == "1m 47s作業しました"
            )
        );
        assert!(!detail.transcript_rows.iter().any(|row| {
            row.title.contains("Terminal")
                || row.title.contains("編集中")
                || row.kind == "tool"
                || row.kind == "editing"
        }));
        assert!(
            detail
                .transcript_rows
                .iter()
                .any(|row| row.kind == "assistant" && row.body.contains("src/workflow.rs"))
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
                    status: crate::protocol::TurnTerminalStatus::Completed,
                    summary: "session completed".to_string(),
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
            .filter(|row| row.kind == "assistant")
            .collect::<Vec<_>>();
        let primary_text = rows
            .iter()
            .filter(|row| row.kind != "work_summary_completed")
            .map(|row| row.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let work_summary = rows
            .iter()
            .find(|row| row.kind == "work_summary_completed")
            .expect("work summary row");

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("完了しました"));
        assert!(rows.iter().any(|row| row.kind == "file_changes"));
        assert!(!primary_text.contains("Turn control projection surface"));
        assert!(!primary_text.contains("Invalid tool arguments"));
        assert!(work_summary.body.contains("中間応答"));
        assert!(completed_desktop_transcript_primary_reading_fixture_passes());
    }

    #[test]
    fn desktop_turn_item_projection_uses_turn_local_sequence() {
        assert!(desktop_turn_item_projection_uses_turn_local_sequence_fixture_passes());
    }

    #[test]
    fn desktop_transcript_row_kind_typed_projection() {
        assert!(desktop_transcript_row_kind_typed_projection_fixture_passes());
    }

    #[test]
    fn completed_work_transcript_keeps_final_answer_and_folds_intermediate_assistant_prose() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(crate::session::RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 2,
            failed_tool_count: 0,
            change_count: 1,
        });
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "まず tests/workflow.contract を作成します。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "次に src/workflow.rs を作成します。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body:
                    "完了しました。src/workflow.rs と tests/workflow.contract を作成し、検証も通りました。"
                        .to_string(),
                message_id: None,
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
            .filter(|row| row.kind == "assistant")
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("完了しました"));
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
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
        state.last_summary = Some(crate::session::RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 2,
            failed_tool_count: 0,
            change_count: 1,
        });
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "検証は成功しました。\n<tool_call>\n<function=shell>\n<parameter=command>\nverify-contract --behavior\n</parameter>\n</function>\n</tool_call>".to_string(),
                message_id: None,
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
            .filter(|row| row.kind == "assistant")
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("<tool_call>"));
        assert!(assistant_rows[0].body.contains("<parameter=command>"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn completed_work_transcript_folds_intermediate_assistant_rows() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(crate::session::RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 3,
            failed_tool_count: 0,
            change_count: 2,
        });
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "作業中です。verification evidence を確認しています。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "完了しました。src/workflow.rs と tests/workflow.contract を作成しました。"
                    .to_string(),
                message_id: None,
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
            .filter(|row| row.kind == "assistant")
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("src/workflow.rs"));
        assert!(
            !rows
                .iter()
                .any(|row| row.body.contains("verification evidence を確認"))
        );
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn completed_work_transcript_preserves_closing_tag_only_pseudo_tool_call_fragment() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(crate::session::RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 3,
            failed_tool_count: 0,
            change_count: 2,
        });
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "workflow_state.ready = true\n</parameter> <parameter=path> src/workflow.rs </parameter> </function> </tool_call>"
                    .to_string(),
                message_id: None,
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
            .filter(|row| row.kind == "assistant")
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("</tool_call>"));
        assert!(assistant_rows[0].body.contains("<parameter=path>"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn completed_work_transcript_preserves_html_escaped_pseudo_tool_call_fragment() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Completed;
        state.last_summary = Some(crate::session::RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 3,
            failed_tool_count: 0,
            change_count: 2,
        });
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "workflow_state.ready = true\n&lt;/parameter&gt; &lt;parameter=path&gt; src/workflow.rs &lt;/parameter&gt; &lt;/function&gt; &lt;/tool_call&gt;"
                    .to_string(),
                message_id: None,
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
            .filter(|row| row.kind == "assistant")
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("&lt;/tool_call"));
        assert!(assistant_rows[0].body.contains("&lt;parameter=path"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn reopened_completed_session_preserves_pseudo_tool_call_closeout_evidence() {
        let project_id = ProjectId::new();
        let session = session_record(project_id, "workflow");
        let mut state = AppState::default();
        state.run_status = RunStatus::Idle;
        state.last_summary = Some(crate::session::RunSummary {
            session_id: session.id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 3,
            failed_tool_count: 0,
            change_count: 2,
        });
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "workflow_state.ready = true\n&lt;/parameter&gt; &lt;parameter=path&gt; src/workflow.rs &lt;/parameter&gt; &lt;/function&gt; &lt;/tool_call&gt;"
                    .to_string(),
                message_id: None,
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
            .filter(|row| row.kind == "assistant")
            .collect::<Vec<_>>();

        assert_eq!(assistant_rows.len(), 1);
        assert!(assistant_rows[0].body.contains("&lt;/tool_call"));
        assert!(assistant_rows[0].body.contains("&lt;parameter=path"));
        assert_ne!(assistant_rows[0].body, "完了しました。");
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn restored_completed_session_folds_intermediate_assistant_rows_without_cleanup() {
        let mut state = AppState::default();
        state.run_status = RunStatus::Idle;
        state.last_summary = Some(crate::session::RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 3,
            failed_tool_count: 0,
            change_count: 2,
        });
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "src/workflow.rs と tests/workflow.contract を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "テスト失敗を修正します。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "完了しました。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
        ];

        let rows = transcript_rows_with_context(&state, None, &[]);

        assert_eq!(rows.iter().filter(|row| row.kind == "assistant").count(), 1);
        assert!(
            !rows
                .iter()
                .any(|row| row.body.contains("テスト失敗を修正します"))
        );
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }
}
