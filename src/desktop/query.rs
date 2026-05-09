use crate::app::App;
use crate::desktop::args::DesktopArgs;
use crate::desktop::models::{DesktopSessionDetail, DesktopSessionRow, DesktopSnapshot};
use crate::error::AppRunError;
use crate::harness::{ReplayReport, ReplayReportStore};
use crate::session::{
    SessionId, SessionRecord, SessionStateSnapshot, SessionStatus, TodoItem, Transcript,
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
    let selected_session_index = select_session_index(&sessions, selected_session_id, false)?;
    build_snapshot(app, sessions, selected_session_index).await
}

pub async fn load_snapshot_continue_last(app: &App) -> Result<DesktopSnapshot, AppRunError> {
    let sessions = recent_sessions(&app.session_service, app.workspace.project_id, 20).await?;
    let selected_session_index = select_session_index(&sessions, None, true)?;
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
        session_rows,
        session_details,
        selected_session_index,
    })
}

pub fn select_session_index(
    sessions: &[SessionRecord],
    session_id: Option<SessionId>,
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
        tool_status_text: format_tool_status_text(state, &session_state, &state.sidebar_todos),
        progress_text: format_progress_text(state),
        run_status_text: format_run_status_text(state, &session_state),
        confirmation_text: format_confirmation_text(state),
        confirmation_visible: state.permission.is_some(),
    }
}

fn format_session_row(session: &SessionRecord) -> String {
    format!(
        "{} [{}] {}",
        truncate_text(&session.title, 28),
        format_session_status(session.status),
        short_session_id(session.id)
    )
}

fn format_session_status(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Idle",
        SessionStatus::Running => "Running",
        SessionStatus::Completed => "Done",
        SessionStatus::AwaitingUser => "Waiting",
        SessionStatus::Failed => "Failed",
    }
}

fn format_transcript_text(state: &AppState) -> String {
    if state.transcript_entries.is_empty() {
        return "No transcript recorded yet.".to_string();
    }
    state
        .transcript_entries
        .iter()
        .map(|entry| {
            let heading = entry_heading(entry.kind, &entry.title);
            if entry.body.trim().is_empty() {
                heading
            } else {
                format!("{heading}\n{}", entry.body.trim())
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_tool_status_text(
    state: &AppState,
    session_state: &SessionStateSnapshot,
    todos: &[TodoItem],
) -> String {
    let mut lines = Vec::new();
    if state.tool_statuses.is_empty() {
        lines.push("Tools: no tool activity recorded.".to_string());
    } else {
        lines.push("Tools:".to_string());
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
        lines.push("Todos:".to_string());
        lines.extend(todos.iter().map(|todo| {
            format!(
                "- [{}] {}",
                format!("{:?}", todo.status).to_lowercase(),
                todo.content
            )
        }));
    }
    if let Some(summary) = &session_state.completion.route_contract_summary {
        lines.push(String::new());
        lines.push(format!("Contract: {summary}"));
    }
    if let Some(handoff) = &session_state.implementation_handoff {
        if !handoff.next_actions.is_empty() {
            lines.push(String::new());
            lines.push("Next:".to_string());
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
        lines.push(format!("Failure: {}", failure.summary));
    }
    lines.join("\n")
}

fn append_replay_summary(tool_status_text: &mut String, report: &ReplayReport) {
    if !tool_status_text.is_empty() {
        tool_status_text.push_str("\n\n");
    }
    tool_status_text.push_str("Replay:\n");
    tool_status_text.push_str(&format!(
        "- status: {}",
        format!("{:?}", report.status).to_lowercase()
    ));
    if let Some(owner) = report.primary_owner {
        tool_status_text.push_str(&format!(
            "\n- primary owner: {}",
            format!("{:?}", owner).to_lowercase()
        ));
    }
    if !report.summary.trim().is_empty() {
        tool_status_text.push_str(&format!("\n- summary: {}", report.summary.trim()));
    }
    if let Some(restart) = &report.restart_point {
        tool_status_text.push_str(&format!("\n- restart: {restart}"));
    }
}

fn format_run_status_text(state: &AppState, session_state: &SessionStateSnapshot) -> String {
    let mut lines = vec![run_status_label(state.run_status).to_string()];
    lines.push(format!("Route: {:?}", session_state.route).to_lowercase());
    lines.push(format!("Phase: {:?}", session_state.process_phase).to_lowercase());
    if let Some(message) = &state.status_message {
        lines.push(format!("Status: {message}"));
    }
    lines.push(format!(
        "Open work: {}",
        session_state.completion.open_work_count
    ));
    if session_state.completion.verification_pending {
        lines.push("Verification: pending".to_string());
    }
    if let Some(blocked) = &session_state.completion.blocked_reason {
        lines.push(format!("Blocked: {blocked}"));
    }
    lines.join("\n")
}

fn format_progress_text(state: &AppState) -> String {
    let progress = &state.progress;
    vec![
        progress.status.clone(),
        format!("Phase: {}", progress.current_phase),
        format!("Step: {}", progress.active_step),
        format!("Model requests: {}", progress.model_requests),
        format!(
            "Tools: {} started / {} completed / {} failed",
            progress.tool_calls_started, progress.tool_calls_completed, progress.tool_calls_failed
        ),
        format!("Rejected proposals: {}", progress.rejected_tool_proposals),
        format!("Compactions: {}", progress.compactions),
        format!("Retries: {}", progress.retries),
    ]
    .join("\n")
}

fn format_confirmation_text(state: &AppState) -> String {
    let Some(permission) = &state.permission else {
        return String::new();
    };
    let targets = if permission.targets.is_empty() {
        "(none)".to_string()
    } else {
        permission.targets.join(", ")
    };
    let risks = if permission.risks.is_empty() {
        "none".to_string()
    } else {
        permission.risks.join(", ")
    };
    format!(
        "{}\n\nTargets: {targets}\nOutside workspace: {}\nRisks: {risks}",
        permission.summary,
        if permission.outside_workspace {
            "yes"
        } else {
            "no"
        }
    )
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "Idle",
        RunStatus::Running => "Running",
        RunStatus::Confirming => "Confirming",
        RunStatus::Completed => "Completed",
        RunStatus::AwaitingUser => "Awaiting User",
        RunStatus::Failed => "Failed",
    }
}

fn entry_heading(kind: TranscriptKind, title: &str) -> String {
    match kind {
        TranscriptKind::User => "User".to_string(),
        TranscriptKind::Assistant => "Assistant".to_string(),
        TranscriptKind::Reasoning => "Reasoning".to_string(),
        TranscriptKind::Tool => format!("Tool - {title}"),
        TranscriptKind::Diff => format!("Diff - {title}"),
        TranscriptKind::System => format!("System - {title}"),
        TranscriptKind::Error => format!("Error - {title}"),
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
