use crate::app::App;
use crate::desktop::args::DesktopArgs;
use crate::desktop::models::{
    DesktopArtifactRow, DesktopCommandRow, DesktopFileChangeRow, DesktopProjectRow,
    DesktopSessionDetail, DesktopSessionRow, DesktopSnapshot, DesktopTranscriptRow,
};
use crate::error::AppRunError;
use crate::harness::{ReplayReport, ReplayReportStore};
use crate::protocol::{FileChangeEvidence, TurnItem, TurnItemPayload};
use crate::session::{
    ChangeKind, MessagePart, ProjectId, ProjectRecord, SessionId, SessionRecord,
    SessionStateSnapshot, SessionStatus, TodoItem, ToolCallStatus, Transcript,
};
use crate::tui::query::{recent_sessions, session_view};
use crate::tui::state::{AppState, RunStatus, TranscriptKind};

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
    let mut session_details = Vec::with_capacity(sessions.len());
    let projects = app.session_service.list_projects(30).await?;
    let hidden_roots =
        internal_desktop_project_roots(app.session_service.store.paths().data_dir.as_path());
    let (project_rows, selected_project_index) = build_project_rows(
        &projects,
        app.workspace.project_id,
        &app.workspace.root,
        &hidden_roots,
    );
    for session in &sessions {
        let view = session_view(&app.session_service, session.id).await?;
        session_rows.push(DesktopSessionRow {
            session_id: session.id,
            label: format_session_row(session),
        });
        let replay_report = app
            .store
            .harness_replay_report_store()
            .latest_report_for_session(session.id)?;
        session_details.push(build_session_detail(
            session,
            view.state,
            view.todos,
            view.transcript,
            view.turn_items,
            replay_report,
        ));
    }
    Ok(DesktopSnapshot {
        workspace_path: app.workspace.root.to_string(),
        provider_label: app.config.model.base_url.clone(),
        model_label: app.config.model.model.clone(),
        command_rows: load_command_rows(&app.workspace.root),
        project_rows,
        selected_project_index,
        session_rows,
        session_details,
        selected_session_index,
    })
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
    let mut detail = build_session_detail_from_app_state(&ui_state);
    detail.session_id = session.id;
    let file_changes = if turn_items.is_empty() {
        file_change_rows_from_transcript(&transcript)
    } else {
        file_change_rows_from_turn_items(&turn_items)
    };
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
    let session_state = state.session_state.clone().unwrap_or_default();
    DesktopSessionDetail {
        session_id: state.current_session_id.unwrap_or_else(SessionId::new),
        transcript_text: format_transcript_text(state),
        transcript_rows: transcript_rows(state),
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

pub fn file_change_rows_from_turn_items(turn_items: &[TurnItem]) -> Vec<DesktopFileChangeRow> {
    let mut rows = Vec::new();
    for item in turn_items {
        if let TurnItemPayload::FileChange {
            changes, summary, ..
        } = &item.payload
        {
            rows.extend(
                changes
                    .iter()
                    .filter(|change| file_change_is_user_visible(change))
                    .map(|change| file_change_row(change, summary.as_str())),
            );
        }
    }
    dedupe_file_change_rows(rows)
}

fn file_change_rows_from_transcript(transcript: &Transcript) -> Vec<DesktopFileChangeRow> {
    let mut rows = Vec::new();
    for message in &transcript.messages {
        for part in &message.parts {
            if let MessagePart::DiffSummary(summary) = &part.payload {
                rows.extend(
                    summary
                        .changes
                        .iter()
                        .filter(|change| file_change_is_user_visible(change))
                        .map(|change| file_change_row(change, summary.summary.as_str())),
                );
            }
        }
    }
    dedupe_file_change_rows(rows)
}

fn file_change_row(change: &FileChangeEvidence, fallback_summary: &str) -> DesktopFileChangeRow {
    let path = change
        .path_after
        .as_ref()
        .or(change.path_before.as_ref())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "(不明なパス)".to_string());
    let label = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(path.as_str())
        .to_string();
    let summary = if change.summary.trim().is_empty() {
        fallback_summary.trim().to_string()
    } else {
        change.summary.trim().to_string()
    };
    DesktopFileChangeRow {
        label,
        path,
        action: change_kind_label(change.kind).to_string(),
        summary,
    }
}

fn file_change_is_user_visible(change: &FileChangeEvidence) -> bool {
    change
        .path_after
        .as_ref()
        .or(change.path_before.as_ref())
        .is_some_and(|path| is_user_visible_artifact_path(path.as_str()))
}

fn artifact_rows_from_file_changes(rows: &[DesktopFileChangeRow]) -> Vec<DesktopArtifactRow> {
    let mut artifacts = rows
        .iter()
        .filter(|row| is_user_visible_artifact_path(&row.path))
        .map(|row| DesktopArtifactRow {
            label: row.label.clone(),
            path: row.path.clone(),
            kind: "ファイル".to_string(),
            action: row.action.clone(),
        })
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    artifacts.dedup_by(|left, right| left.path == right.path);
    artifacts
}

fn is_user_visible_artifact_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    !normalized.contains("/__pycache__/")
        && !normalized.starts_with("__pycache__/")
        && !normalized.ends_with(".pyc")
}

fn dedupe_file_change_rows(rows: Vec<DesktopFileChangeRow>) -> Vec<DesktopFileChangeRow> {
    let mut deduped: Vec<DesktopFileChangeRow> = Vec::new();
    for row in rows {
        if let Some(existing) = deduped
            .iter_mut()
            .find(|existing| existing.path == row.path && existing.action == row.action)
        {
            if !row.summary.trim().is_empty() {
                existing.summary = row.summary;
            }
        } else {
            deduped.push(row);
        }
    }
    deduped
}

fn format_file_change_summary(rows: &[DesktopFileChangeRow]) -> String {
    if rows.is_empty() {
        return "ファイル変更はまだありません。".to_string();
    }
    let added = rows.iter().filter(|row| row.action == "追加").count();
    let updated = rows.iter().filter(|row| row.action == "更新").count();
    let deleted = rows.iter().filter(|row| row.action == "削除").count();
    let moved = rows.iter().filter(|row| row.action == "移動").count();
    let mut lines = vec![format!(
        "{}件のファイル変更（追加{} / 更新{} / 削除{} / 移動{}）",
        rows.len(),
        added,
        updated,
        deleted,
        moved
    )];
    lines.extend(rows.iter().take(8).map(|row| {
        if row.summary.trim().is_empty() {
            format!("- [{}] {}", row.action, row.path)
        } else {
            format!("- [{}] {} - {}", row.action, row.path, row.summary)
        }
    }));
    lines.join("\n")
}

pub fn format_artifact_preview(
    artifact: Option<&DesktopArtifactRow>,
    changes: &[DesktopFileChangeRow],
) -> String {
    let Some(artifact) = artifact else {
        return "アーティファクトは選択されていません。".to_string();
    };
    let mut lines = vec![
        format!("アーティファクト: {}", artifact.label),
        format!("パス: {}", artifact.path),
        format!("種別: {}", artifact.kind),
        format!("操作: {}", artifact.action),
    ];
    if let Some(change) = changes.iter().find(|change| change.path == artifact.path) {
        if !change.summary.trim().is_empty() {
            lines.push(String::new());
            lines.push(change.summary.clone());
        }
    }
    lines.push(String::new());
    lines.push(
        "差分はセッション履歴のファイル変更から確認できます。Undo は安全契約を増やすため、この画面には露出していません。"
            .to_string(),
    );
    lines.join("\n")
}

fn change_kind_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Add => "追加",
        ChangeKind::Update => "更新",
        ChangeKind::Delete => "削除",
        ChangeKind::Move => "移動",
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

fn format_session_row(session: &SessionRecord) -> String {
    format!(
        "{} [{}] {}",
        truncate_text(&session.title, 24),
        format_session_status(session.status),
        short_session_id(session.id)
    )
}

fn format_session_status(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "待機中",
        SessionStatus::Running => "実行中",
        SessionStatus::Completed => "完了",
        SessionStatus::AwaitingUser => "確認待ち",
        SessionStatus::Failed => "失敗",
    }
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

fn transcript_rows(state: &AppState) -> Vec<DesktopTranscriptRow> {
    let mut rows = if state.transcript_entries.is_empty() {
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
            .map(|(index, entry)| DesktopTranscriptRow {
                kind: transcript_kind_key(entry.kind).to_string(),
                step: format!("{:02}", index + 1),
                title: entry_heading(entry.kind, &entry.title),
                body: entry.body.trim().to_string(),
            })
            .collect::<Vec<_>>()
    };

    if matches!(state.run_status, RunStatus::Running | RunStatus::Confirming) {
        rows.push(DesktopTranscriptRow {
            kind: "running".to_string(),
            step: format!("{:02}", rows.len() + 1),
            title: if state.run_status == RunStatus::Confirming {
                "確認待ち".to_string()
            } else {
                "実行中".to_string()
            },
            body: format!(
                "{}\nフェーズ: {}\nコマンド: {}件完了 / {}件開始",
                state.progress.active_step,
                state.progress.current_phase,
                state.progress.tool_calls_completed,
                state.progress.tool_calls_started
            ),
        });
    }

    if !state.sidebar_todos.is_empty() {
        rows.push(DesktopTranscriptRow {
            kind: "tasks".to_string(),
            step: format!("{:02}", rows.len() + 1),
            title: task_summary_title(&state.sidebar_todos),
            body: format_todo_rows(&state.sidebar_todos),
        });
    }

    if !state.tool_statuses.is_empty() {
        rows.push(DesktopTranscriptRow {
            kind: "summary".to_string(),
            step: format!("{:02}", rows.len() + 1),
            title: format_command_summary_title(&state.tool_statuses),
            body: state
                .tool_statuses
                .iter()
                .take(10)
                .map(|tool| {
                    let status = format!("{:?}", tool.status).to_lowercase();
                    let detail = tool
                        .summary
                        .as_ref()
                        .or(tool.error.as_ref())
                        .map(|value| format!(" - {}", value.trim()))
                        .unwrap_or_default();
                    format!("[{status}] {} - {}{}", tool.tool, tool.title, detail)
                })
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }

    if let Some(summary) = &state.last_summary {
        rows.push(DesktopTranscriptRow {
            kind: if summary.failed_tool_count > 0 {
                "error".to_string()
            } else {
                "summary".to_string()
            },
            step: format!("{:02}", rows.len() + 1),
            title: "完了サマリ".to_string(),
            body: format!(
                "status: {:?}\nfinish_reason: {}\ntools: {} executed, {} failed\nfile changes: {}",
                summary.status,
                summary
                    .finish_reason
                    .as_ref()
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|| "none".to_string()),
                summary.tool_call_count,
                summary.failed_tool_count,
                summary.change_count
            ),
        });
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
        ];

        let text = format_transcript_text(&state);

        assert!(text.contains("[01] ユーザー依頼"));
        assert!(text.contains("[02] コマンド / ツール - write"));
        assert!(text.contains("[03] ファイル変更 - File changes"));
        assert!(!text.contains("===="));

        state.tool_statuses = vec![crate::tui::state::ToolStatusView {
            tool_call_id: crate::session::ToolCallId::new(),
            tool: crate::tool::ToolName::Shell,
            title: "python -m unittest".to_string(),
            status: ToolCallStatus::Completed,
            summary: Some("tests passed".to_string()),
            error: None,
        }];
        let rows = transcript_rows(&state);
        assert!(rows.iter().any(|row| row.title == "1件のコマンドを実行"));

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
        assert!(rows.iter().any(|row| row.title == "実行中"));
        assert!(rows.iter().any(|row| row.title.starts_with("タスク進捗")));
        assert!(rows.iter().any(|row| row.title == "完了サマリ"));
    }
}
