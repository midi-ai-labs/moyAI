use std::collections::BTreeSet;

use camino::Utf8Path;

use crate::agent::state_lifecycle::persist_state_update;
use crate::error::{AgentError, RuntimeError};
use crate::protocol::{ProtocolEventStore, RuntimeEventMsg, TurnId};
use crate::runtime::RunEventSink;
use crate::session::{
    AssistantMessageMeta, FinishReason, MessageId, MessageMetadata, MessageRole, NewMessage,
    NewSession, ProjectId, ProjectRepository, RunEvent, RunSummary, SessionId, SessionRepository,
    SessionStatus, TokenAccountingState, TokenUsage,
};
use crate::storage::{SqliteSessionRepository, SqliteStore, StoragePaths};

pub(crate) async fn persist_provider_token_accounting(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    context_window: u32,
    token_usage: Option<&TokenUsage>,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<(), AgentError> {
    let Some(token_usage) = token_usage else {
        return Ok(());
    };
    let mut state = session_repo.get_state(session_id).await?;
    state.token_accounting = TokenAccountingState::from_provider_usage(context_window, token_usage);
    persist_state_update(session_repo, session_id, &state, protocol_turn_id, sink).await
}

pub(crate) fn terminal_assistant_metadata(
    model: &str,
    base_url: &str,
    finish_reason: Option<FinishReason>,
    token_usage: Option<TokenUsage>,
) -> MessageMetadata {
    MessageMetadata::Assistant(AssistantMessageMeta {
        model: model.to_string(),
        base_url: base_url.to_string(),
        finish_reason,
        token_usage,
        summary: false,
    })
}

pub(crate) fn terminal_completed_event(
    session_id: SessionId,
    finish_reason: Option<FinishReason>,
) -> RunEvent {
    RunEvent::SessionCompleted {
        session_id,
        finish_reason,
    }
}

pub(crate) fn terminal_interrupted_event(session_id: SessionId, reason: &str) -> RunEvent {
    RunEvent::SessionInterrupted {
        session_id,
        reason: reason.to_string(),
    }
}

pub(crate) fn terminal_failed_event(session_id: SessionId, message: &str) -> RunEvent {
    RunEvent::SessionFailed {
        session_id,
        message: message.to_string(),
    }
}

pub(crate) fn terminal_run_summary(
    session_id: SessionId,
    assistant_message_id: MessageId,
    status: SessionStatus,
    finish_reason: Option<FinishReason>,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
) -> RunSummary {
    RunSummary {
        session_id,
        assistant_message_id: Some(assistant_message_id),
        status,
        finish_reason,
        tool_call_count,
        failed_tool_count,
        change_count,
    }
}

pub(crate) async fn complete_turn(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    assistant_message_id: MessageId,
    model: &str,
    base_url: &str,
    finish_reason: Option<FinishReason>,
    token_usage: Option<TokenUsage>,
    context_window: u32,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    let terminal_event = terminal_completed_event(session_id, finish_reason.clone());
    persist_provider_token_accounting(
        session_repo,
        session_id,
        context_window,
        token_usage.as_ref(),
        protocol_turn_id,
        sink,
    )
    .await?;
    session_repo
        .update_message_metadata_and_status_with_protocol_event(
            session_id,
            assistant_message_id,
            &terminal_assistant_metadata(model, base_url, finish_reason.clone(), token_usage),
            SessionStatus::Completed,
            &terminal_event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(terminal_event)?;
    Ok(terminal_run_summary(
        session_id,
        assistant_message_id,
        SessionStatus::Completed,
        finish_reason,
        tool_call_count,
        failed_tool_count,
        change_count,
    ))
}

pub(crate) async fn interrupt_turn(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    assistant_message_id: MessageId,
    model: &str,
    base_url: &str,
    reason: &str,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    let terminal_event = terminal_interrupted_event(session_id, reason);
    session_repo
        .update_message_metadata_and_status_with_protocol_event(
            session_id,
            assistant_message_id,
            &terminal_assistant_metadata(model, base_url, Some(FinishReason::Cancelled), None),
            SessionStatus::Cancelled,
            &terminal_event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(terminal_event)?;
    Ok(terminal_run_summary(
        session_id,
        assistant_message_id,
        SessionStatus::Cancelled,
        Some(FinishReason::Cancelled),
        tool_call_count,
        failed_tool_count,
        change_count,
    ))
}

pub(crate) async fn fail_turn(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    assistant_message_id: MessageId,
    model: &str,
    base_url: &str,
    message: &str,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    let terminal_event = terminal_failed_event(session_id, message);
    session_repo
        .update_message_metadata_and_status_with_protocol_event(
            session_id,
            assistant_message_id,
            &terminal_assistant_metadata(model, base_url, Some(FinishReason::Error), None),
            SessionStatus::Failed,
            &terminal_event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(terminal_event)?;
    Ok(terminal_run_summary(
        session_id,
        assistant_message_id,
        SessionStatus::Failed,
        Some(FinishReason::Error),
        tool_call_count,
        failed_tool_count,
        change_count,
    ))
}

pub(crate) fn terminal_turn_projection_fixture_passes(
    fixture_model: &str,
    fixture_base_url: &str,
) -> bool {
    let session_id = SessionId::new();
    let message_id = MessageId::new();
    let token_usage = TokenUsage {
        prompt_tokens: 11,
        completion_tokens: 3,
        total_tokens: 14,
        reasoning_tokens: Some(2),
    };
    let completed_metadata = terminal_assistant_metadata(
        fixture_model,
        fixture_base_url,
        Some(FinishReason::Stop),
        Some(token_usage.clone()),
    );
    let interrupted_metadata = terminal_assistant_metadata(
        fixture_model,
        fixture_base_url,
        Some(FinishReason::Cancelled),
        None,
    );
    let failed_metadata = terminal_assistant_metadata(
        fixture_model,
        fixture_base_url,
        Some(FinishReason::Error),
        None,
    );
    let completed_event = terminal_completed_event(session_id, Some(FinishReason::Stop));
    let interrupted_event = terminal_interrupted_event(session_id, "cancel requested");
    let failed_event = terminal_failed_event(session_id, "provider error");
    let completed_summary = terminal_run_summary(
        session_id,
        message_id,
        SessionStatus::Completed,
        Some(FinishReason::Stop),
        2,
        1,
        3,
    );
    let interrupted_summary = terminal_run_summary(
        session_id,
        message_id,
        SessionStatus::Cancelled,
        Some(FinishReason::Cancelled),
        2,
        1,
        3,
    );
    let failed_summary = terminal_run_summary(
        session_id,
        message_id,
        SessionStatus::Failed,
        Some(FinishReason::Error),
        2,
        1,
        3,
    );
    matches!(
        completed_metadata,
        MessageMetadata::Assistant(AssistantMessageMeta {
            model,
            base_url,
            finish_reason: Some(FinishReason::Stop),
            token_usage: Some(TokenUsage { total_tokens: 14, .. }),
            summary: false,
        }) if model == fixture_model && base_url == fixture_base_url
    ) && matches!(
        interrupted_metadata,
        MessageMetadata::Assistant(AssistantMessageMeta {
            finish_reason: Some(FinishReason::Cancelled),
            token_usage: None,
            summary: false,
            ..
        })
    ) && matches!(
        failed_metadata,
        MessageMetadata::Assistant(AssistantMessageMeta {
            finish_reason: Some(FinishReason::Error),
            token_usage: None,
            summary: false,
            ..
        })
    ) && matches!(
        completed_event,
        RunEvent::SessionCompleted {
            finish_reason: Some(FinishReason::Stop),
            ..
        }
    ) && matches!(
        interrupted_event,
        RunEvent::SessionInterrupted { reason, .. } if reason == "cancel requested"
    ) && matches!(
        failed_event,
        RunEvent::SessionFailed { message, .. } if message == "provider error"
    ) && completed_summary.status == SessionStatus::Completed
        && completed_summary.finish_reason == Some(FinishReason::Stop)
        && interrupted_summary.status == SessionStatus::Cancelled
        && interrupted_summary.finish_reason == Some(FinishReason::Cancelled)
        && failed_summary.status == SessionStatus::Failed
        && failed_summary.finish_reason == Some(FinishReason::Error)
        && completed_summary.assistant_message_id == Some(message_id)
        && completed_summary.tool_call_count == 2
        && completed_summary.failed_tool_count == 1
        && completed_summary.change_count == 3
}

pub(crate) fn terminal_token_accounting_sequence_fixture_passes(
    fixture_model: &str,
    fixture_base_url: &str,
) -> bool {
    struct CountingSink {
        next_sequence_no: i64,
    }

    impl RunEventSink for CountingSink {
        fn emit(&mut self, _event: RunEvent) -> Result<(), RuntimeError> {
            self.next_sequence_no += 1;
            Ok(())
        }

        fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
            let sequence_no = self.next_sequence_no;
            self.next_sequence_no += 1;
            Some(sequence_no)
        }

        fn emit_pre_recorded(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
            let _ = event;
            Ok(())
        }
    }

    let unique = format!(
        "moyai-terminal-token-accounting-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    );
    let root_path = std::env::temp_dir().join(unique);
    let Ok(data_dir) = camino::Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    let paths = StoragePaths {
        data_dir: data_dir.clone(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let worker_paths = paths.clone();
    let fixture_model = fixture_model.to_string();
    let fixture_base_url = fixture_base_url.to_string();
    let result = std::thread::spawn(move || -> Result<bool, RuntimeError> {
        let store = SqliteStore::open(&worker_paths)
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        store
            .migrate()
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        let project_repo = store.project_repo();
        let session_repo = store.session_repo();
        let protocol_store = store.protocol_event_store();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        runtime.block_on(async {
            let project_id = ProjectId::new();
            let workspace_root = Utf8Path::new("C:/workspace/terminal-token-accounting");
            project_repo
                .upsert_project(
                    project_id,
                    workspace_root,
                    "Terminal Token Accounting",
                    "none",
                )
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let session = session_repo
                .create_session(NewSession {
                    project_id,
                    title: "terminal token accounting".to_string(),
                    cwd: workspace_root.to_path_buf(),
                    model: fixture_model.clone(),
                    base_url: fixture_base_url.clone(),
                    access_mode: crate::config::AccessMode::Default,
                })
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let turn_id = TurnId::new();
            let (assistant, _) = session_repo
                .append_assistant_message_with_protocol_start(
                    NewMessage {
                        session_id: session.id,
                        parent_message_id: None,
                        role: MessageRole::Assistant,
                        metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                            model: fixture_model.clone(),
                            base_url: fixture_base_url.clone(),
                            finish_reason: None,
                            token_usage: None,
                            summary: false,
                        }),
                    },
                    turn_id,
                    Some(0),
                    fixture_model.clone(),
                )
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let mut sink = CountingSink {
                next_sequence_no: 1,
            };
            let token_usage = TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
                total_tokens: 12,
                reasoning_tokens: None,
            };
            persist_provider_token_accounting(
                &session_repo,
                session.id,
                131_072,
                Some(&token_usage),
                turn_id,
                &mut sink,
            )
            .await
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let terminal_event = RunEvent::SessionCompleted {
                session_id: session.id,
                finish_reason: Some(FinishReason::Stop),
            };
            session_repo
                .update_message_metadata_and_status_with_protocol_event(
                    session.id,
                    assistant.id,
                    &MessageMetadata::Assistant(AssistantMessageMeta {
                        model: fixture_model,
                        base_url: fixture_base_url,
                        finish_reason: Some(FinishReason::Stop),
                        token_usage: Some(token_usage),
                        summary: false,
                    }),
                    SessionStatus::Completed,
                    &terminal_event,
                    turn_id,
                    sink.reserve_protocol_sequence_no(),
                )
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            sink.emit_pre_recorded(terminal_event)?;
            let events = protocol_store
                .list_runtime_events(session.id, turn_id)
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let unique_sequence_count = events
                .iter()
                .map(|event| event.sequence_no)
                .collect::<BTreeSet<_>>()
                .len();
            Ok(events.len() == unique_sequence_count
                && events.last().is_some_and(|event| {
                    matches!(event.msg, RuntimeEventMsg::TurnCompleted { .. })
                })
                && events
                    .iter()
                    .any(|event| matches!(event.msg, RuntimeEventMsg::Warning { .. })))
        })
    })
    .join()
    .unwrap_or_else(|_| {
        Err(RuntimeError::Message(
            "terminal token accounting fixture worker panicked".to_string(),
        ))
    });
    let _ = std::fs::remove_dir_all(data_dir.as_std_path());
    result.unwrap_or(false)
}
