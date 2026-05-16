use crate::app::App;
use crate::desktop::args::{DesktopArgs, quick_chat_workspace_directory};
use crate::desktop::models::{
    DesktopCommandRow, DesktopFileChangeRow, DesktopProjectRow, DesktopSessionDetail,
    DesktopSessionRow, DesktopSnapshot, DesktopTranscriptRow, format_session_status,
};
use crate::error::AppRunError;
use crate::harness::ReplayReport;
use crate::session::{
    ProjectId, ProjectRecord, SessionId, SessionRecord, SessionStateSnapshot, SessionStatus,
    TodoItem, ToolCallStatus, Transcript,
};
use crate::tui::query::{recent_sessions, session_view};
use crate::tui::state::{AppState, RunStatus, TranscriptKind};

use super::artifact_projection::{
    artifact_rows_from_file_changes, file_change_rows_from_transcript,
    file_change_rows_from_turn_items_with_root, format_file_change_summary,
};
pub use super::artifact_projection::{file_change_rows_from_turn_items, format_artifact_preview};

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
    ),
    AppRunError,
> {
    let view = session_view(&app.session_service, session_id).await?;
    Ok((
        view.session,
        view.transcript,
        view.turn_items,
        view.state,
        view.todos,
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
        session_rows.push(build_session_row(session));
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
    Ok(sessions.iter().map(build_session_row).collect())
}

fn build_session_row(session: &SessionRecord) -> DesktopSessionRow {
    DesktopSessionRow::from_session(session)
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
                label: current_path
                    .file_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| current_path.to_string()),
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
    let name = if project.display_name.trim().is_empty() {
        project
            .root_path
            .file_name()
            .map(str::to_string)
            .unwrap_or_else(|| project.root_path.to_string())
    } else {
        project.display_name.clone()
    };
    truncate_text(&name, 34)
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
    transcript: Transcript,
    turn_items: Vec<crate::protocol::TurnItem>,
    replay_report: Option<ReplayReport>,
) -> DesktopSessionDetail {
    let mut ui_state = AppState::default();
    if turn_items.is_empty() {
        ui_state.load_transcript(&transcript, state.clone(), todos.clone());
    } else {
        ui_state.load_turn_items(session, &turn_items, state.clone(), todos.clone());
    }
    let file_changes = if turn_items.is_empty() {
        file_change_rows_from_transcript(&transcript)
    } else {
        file_change_rows_from_turn_items_with_root(&turn_items, Some(session.cwd.as_path()))
    };
    let mut detail = build_session_detail_from_app_state(&ui_state);
    detail.session_id = session.id;
    detail.transcript_rows = transcript_rows_with_context(&ui_state, Some(session), &file_changes);
    detail.artifacts = artifact_rows_from_file_changes(&file_changes);
    detail.file_change_summary_text = format_file_change_summary(&file_changes);
    detail.artifact_preview_text = format_artifact_preview(detail.artifacts.first(), &file_changes);
    detail.file_changes = file_changes;
    if let Some(report) = replay_report {
        append_replay_summary(&mut detail.tool_status_text, &report);
    }
    detail
}

pub fn build_session_detail_from_app_state(state: &AppState) -> DesktopSessionDetail {
    build_session_detail_from_app_state_with_session(state, None)
}

pub fn build_session_detail_from_app_state_with_session(
    state: &AppState,
    session: Option<&SessionRecord>,
) -> DesktopSessionDetail {
    let session_state = state.session_state.clone().unwrap_or_default();
    DesktopSessionDetail {
        session_id: state.current_session_id.unwrap_or_else(SessionId::new),
        transcript_text: format_transcript_text(state),
        transcript_rows: transcript_rows_with_context(state, session, &[]),
        tool_status_text: format_tool_status_text(state, &session_state, &state.sidebar_todos),
        progress_text: format_progress_text(state),
        run_status_text: format_run_status_text(state, &session_state),
        confirmation_text: format_confirmation_text(state),
        confirmation_visible: state.permission.is_some(),
        artifacts: Vec::new(),
        file_changes: Vec::new(),
        file_change_summary_text: "ファイル変更はまだありません。".to_string(),
        artifact_preview_text: "アーティファクトは選択されていません。".to_string(),
    }
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
        vec![DesktopTranscriptRow {
            kind: "system".to_string(),
            step: "00".to_string(),
            title: "履歴はまだありません".to_string(),
            body: "依頼を送信すると、ユーザー入力、ツール実行、ファイル変更、最終応答がここに並びます。".to_string(),
        }]
    } else {
        state
            .transcript_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let kind = transcript_kind_key(entry.kind);
                if is_internal_transcript_projection(kind, &entry.title) {
                    return None;
                }
                Some(DesktopTranscriptRow {
                    kind: kind.to_string(),
                    step: format!("{:02}", index + 1),
                    title: entry_heading(entry.kind, &entry.title),
                    body: entry.body.trim().to_string(),
                })
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
            .position(|row| row.kind == "assistant")
            .unwrap_or(rows.len());
        rows.insert(insert_index, work_summary);
    }
    normalize_completed_pseudo_tool_call_closeout(&mut rows, terminal);
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
    let last_assistant_index = rows
        .iter()
        .rposition(|row| row.kind == "assistant" && !row.body.trim().is_empty());
    let assistant_count = rows.iter().filter(|row| row.kind == "assistant").count();
    if assistant_count <= 1 {
        return rows;
    }
    rows.into_iter()
        .enumerate()
        .filter_map(|(index, row)| {
            if row.kind == "assistant" && Some(index) != last_assistant_index {
                None
            } else {
                Some(row)
            }
        })
        .collect()
}

fn normalize_completed_pseudo_tool_call_closeout(
    rows: &mut Vec<DesktopTranscriptRow>,
    terminal: bool,
) {
    if !terminal {
        return;
    }
    let last_assistant_index = rows
        .iter()
        .rposition(|row| row.kind == "assistant" && !row.body.trim().is_empty());
    let mut normalized = Vec::with_capacity(rows.len());
    for (index, mut row) in rows.drain(..).enumerate() {
        if row.kind == "assistant" && transcript_body_is_pseudo_tool_call_closeout(row.body.trim())
        {
            if Some(index) == last_assistant_index {
                row.body = "完了しました。".to_string();
                normalized.push(row);
            }
            continue;
        }
        normalized.push(row);
    }
    *rows = normalized;
}

fn transcript_body_is_pseudo_tool_call_closeout(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("<tool_call>")
        || lower.contains("</tool_call>")
        || lower.contains("&lt;tool_call")
        || lower.contains("&lt;/tool_call")
        || lower.contains("<function=")
        || lower.contains("</function>")
        || lower.contains("&lt;function=")
        || lower.contains("&lt;/function")
        || lower.contains("<parameter=command>")
        || lower.contains("<parameter=path>")
        || lower.contains("&lt;parameter=command")
        || lower.contains("&lt;parameter=path")
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

fn is_internal_transcript_projection(kind: &str, title: &str) -> bool {
    matches!(kind, "tool" | "summary" | "diff" | "reasoning" | "editing")
        || matches!(kind, "system")
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
        RunStatus::Running | RunStatus::Confirming => "work_summary_running",
        RunStatus::Failed => "work_summary_failed",
        RunStatus::Cancelled => "work_summary_cancelled",
        _ => "work_summary_completed",
    };
    Some(DesktopTranscriptRow {
        kind: kind.to_string(),
        step: String::new(),
        title: work_summary_title(state, session),
        body: work_summary_body(state, file_changes),
    })
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

fn format_compact_tool_rows(tools: &[crate::tui::state::ToolStatusView]) -> String {
    tools
        .iter()
        .take(8)
        .map(|tool| format!("- [{}] {}", tool_call_status_label(tool.status), tool.title))
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
        .filter(|todo| format!("{:?}", todo.status) == "Completed")
        .count();
    let blocked = todos
        .iter()
        .filter(|todo| format!("{:?}", todo.status) == "Blocked")
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
            let marker = match format!("{:?}", todo.status).as_str() {
                "Completed" => "✓",
                "InProgress" => "●",
                "Blocked" => "!",
                _ => "○",
            };
            format!(
                "{marker} {}  (状態: {} / 種別: {:?})",
                todo.content,
                todo_status_label(&format!("{:?}", todo.status)),
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

fn transcript_kind_key(kind: TranscriptKind) -> &'static str {
    match kind {
        TranscriptKind::User => "user",
        TranscriptKind::Assistant => "assistant",
        TranscriptKind::Reasoning => "reasoning",
        TranscriptKind::Editing => "editing",
        TranscriptKind::Tool => "tool",
        TranscriptKind::CommandSummary => "summary",
        TranscriptKind::Diff => "diff",
        TranscriptKind::System => "system",
        TranscriptKind::Error => "error",
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
        lines.extend(todos.iter().map(|todo| {
            format!(
                "- [{}] {}",
                todo_status_label(&format!("{:?}", todo.status)),
                todo.content
            )
        }));
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
    format!(
        "{}\n\n対象: {targets}\nワークスペース外: {}\nリスク: {risks}",
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

fn todo_status_label(status: &str) -> &'static str {
    match status {
        "Completed" => "完了",
        "InProgress" => "進行中",
        "Blocked" => "ブロック",
        _ => "未着手",
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
        SessionRecord {
            id: SessionId::new(),
            project_id,
            title: title.to_string(),
            status: SessionStatus::Completed,
            cwd: Utf8PathBuf::from(format!("C:/workspace/{title}")),
            model: "model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: Some(2),
        }
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
            display_name: "other".to_string(),
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
    fn file_change_rows_normalize_workspace_paths_and_collapse_session_edits() {
        let session_id = SessionId::new();
        let workspace_root = Utf8PathBuf::from("C:/workspace/calculator");
        let turn_items = vec![TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id: crate::protocol::TurnId::new(),
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::FileChange {
                change_ids: vec![crate::session::ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from(
                            "C:/workspace/calculator/calculator.py",
                        )),
                        path_after: Some(Utf8PathBuf::from(
                            "C:/workspace/calculator/calculator.py",
                        )),
                        summary: "Updated calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from(
                            "C:/workspace/calculator/test_calculator.py",
                        )),
                        path_after: Some(Utf8PathBuf::from(
                            "C:/workspace/calculator/test_calculator.py",
                        )),
                        summary: "Updated test_calculator.py".to_string(),
                    },
                ],
                summary: "Updated files".to_string(),
            },
        }];

        let rows = file_change_rows_from_turn_items_with_root(&turn_items, Some(&workspace_root));

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, "calculator.py");
        assert_eq!(rows[0].action, "追加");
        assert_eq!(rows[0].summary, "Updated calculator.py");
        assert_eq!(rows[1].path, "test_calculator.py");
        assert_eq!(rows[1].action, "追加");
        assert_eq!(rows[1].summary, "Updated test_calculator.py");
    }

    #[test]
    fn file_change_rows_hide_runtime_cache_files_from_user_history() {
        let session_id = SessionId::new();
        let turn_items = vec![TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id: crate::protocol::TurnId::new(),
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::FileChange {
                change_ids: vec![crate::session::ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("__pycache__/space_invader.pyc")),
                        path_after: Some(Utf8PathBuf::from("__pycache__/space_invader.pyc")),
                        summary: "Updated runtime cache".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: crate::session::ChangeId::new(),
                        kind: ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("space_invader.py")),
                        path_after: Some(Utf8PathBuf::from("space_invader.py")),
                        summary: "Updated game logic".to_string(),
                    },
                ],
                summary: "Updated files".to_string(),
            },
        }];

        let rows = file_change_rows_from_turn_items(&turn_items);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "space_invader.py");
    }

    #[test]
    fn artifact_rows_hide_runtime_cache_files() {
        let rows = vec![
            DesktopFileChangeRow {
                label: "calculator.py".to_string(),
                path: "calculator.py".to_string(),
                action: "add".to_string(),
                summary: String::new(),
            },
            DesktopFileChangeRow {
                label: "calculator.cpython-313.pyc".to_string(),
                path: "__pycache__/calculator.cpython-313.pyc".to_string(),
                action: "add".to_string(),
                summary: String::new(),
            },
        ];

        let artifacts = artifact_rows_from_file_changes(&rows);

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path, "calculator.py");
    }

    #[test]
    fn transcript_text_projects_chat_events_as_scannable_sections() {
        let mut state = AppState::default();
        state.transcript_entries = vec![
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: "create calculator.py".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Tool,
                title: "write".to_string(),
                body: "calculator.py [Completed]".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Diff,
                title: "File changes".to_string(),
                body: "Added calculator.py".to_string(),
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
            title: "python -m unittest".to_string(),
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
        state.progress.active_step = "Running python -m unittest".to_string();
        state.progress.current_phase = "tool".to_string();
        state.sidebar_todos = vec![
            TodoItem::simple(
                "calculator.pyを作成",
                crate::session::TodoStatus::Completed,
                crate::session::TodoPriority::High,
            ),
            TodoItem::simple(
                "unit testを実行",
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
        let mut session = session_record(project_id, "calculator");
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
                    text: "make a calculator".to_string(),
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
                    title: "python -m unittest".to_string(),
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
                    text: "calculator.pyを追加しました。".to_string(),
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
                .any(|row| row.kind == "assistant" && row.body.contains("calculator.py"))
        );
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
                body: "calculator.py と test_calculator.py を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "まず test_calculator.py を作成します。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "次に calculator.py を作成します。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "完了しました。calculator.py と test_calculator.py を作成し、テストも通りました。".to_string(),
                message_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "python -m unittest".to_string(),
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
                .any(|row| row.body.contains("まず test_calculator.py"))
        );
    }

    #[test]
    fn completed_work_transcript_replaces_pseudo_tool_call_closeout_body() {
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
                body: "calculator.py と test_calculator.py を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "テストは成功しました。\n<tool_call>\n<function=shell>\n<parameter=command>\nGet-Content calculator.py -Head 5\n</parameter>\n</function>\n</tool_call>".to_string(),
                message_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "python -m unittest".to_string(),
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
        assert_eq!(assistant_rows[0].body, "完了しました。");
        assert!(!rows.iter().any(|row| row.body.contains("<tool_call>")));
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn completed_work_transcript_removes_intermediate_pseudo_tool_call_rows() {
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
                body: "calculator.py と test_calculator.py を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "</parameter>\n</function>\n</tool_call>".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "完了しました。calculator.py と test_calculator.py を作成しました。"
                    .to_string(),
                message_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "python -m unittest".to_string(),
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
        assert!(assistant_rows[0].body.contains("calculator.py"));
        assert!(!rows.iter().any(|row| row.body.contains("</tool_call>")));
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn completed_work_transcript_replaces_closing_tag_only_pseudo_tool_call_fragment() {
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
                body: "calculator.py と test_calculator.py を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "if name == \"main\": main()\n</parameter> <parameter=path> calculator.py </parameter> </function> </tool_call>"
                    .to_string(),
                message_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "python -m unittest".to_string(),
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
        assert_eq!(assistant_rows[0].body, "完了しました。");
        assert!(!rows.iter().any(|row| row.body.contains("</tool_call>")));
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn completed_work_transcript_replaces_html_escaped_pseudo_tool_call_fragment() {
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
                body: "calculator.py と test_calculator.py を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "if name == \"main\": main()\n&lt;/parameter&gt; &lt;parameter=path&gt; calculator.py &lt;/parameter&gt; &lt;/function&gt; &lt;/tool_call&gt;"
                    .to_string(),
                message_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "python -m unittest".to_string(),
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
        assert_eq!(assistant_rows[0].body, "完了しました。");
        assert!(!rows.iter().any(|row| row.body.contains("&lt;/tool_call")));
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn reopened_completed_session_uses_session_status_for_pseudo_tool_call_cleanup() {
        let project_id = ProjectId::new();
        let session = session_record(project_id, "calculator");
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
                body: "calculator.py と test_calculator.py を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "if name == \"main\": main()\n&lt;/parameter&gt; &lt;parameter=path&gt; calculator.py &lt;/parameter&gt; &lt;/function&gt; &lt;/tool_call&gt;"
                    .to_string(),
                message_id: None,
                tool_call_id: None,
            },
        ];
        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "python -m unittest".to_string(),
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
        assert_eq!(assistant_rows[0].body, "完了しました。");
        assert!(!rows.iter().any(|row| row.body.contains("&lt;/tool_call")));
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }

    #[test]
    fn restored_completed_session_uses_last_summary_status_for_pseudo_tool_call_cleanup() {
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
                body: "calculator.py と test_calculator.py を作成".to_string(),
                message_id: None,
                tool_call_id: None,
            },
            crate::tui::state::TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "テスト失敗を修正します。\n<tool_call>\n<function=write>\n<parameter=path>calculator.py</parameter>\n</function>\n</tool_call>"
                    .to_string(),
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
        assert!(!rows.iter().any(|row| row.body.contains("<tool_call>")));
        assert!(rows.iter().any(|row| row.kind == "work_summary_completed"));
    }
}
