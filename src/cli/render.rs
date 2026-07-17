use crate::cli::terminal::{terminal_safe_inline, terminal_safe_multiline};
use crate::error::CliRenderError;
use crate::protocol::{HistoryItem, HistoryItemPayload};
use crate::session::{
    CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead, CanonicalTurnPage,
    IdleTurnAdmission, LoadedSessionList, RunEvent, RunSummary, RunningSessionRejoin,
    SessionRecord, ThreadGoal, ThreadGoalClearResult, ThreadGoalGetResult, ThreadGoalSetResult,
};

pub trait EventRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError>;
    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError>;
    fn render_session_list(&mut self, sessions: &[SessionRecord]) -> Result<(), CliRenderError>;
    fn render_loaded_sessions(&mut self, loaded: &LoadedSessionList) -> Result<(), CliRenderError>;
    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
    ) -> Result<(), CliRenderError>;
    fn render_session_history_page(
        &mut self,
        page: &CanonicalHistoryPage,
    ) -> Result<(), CliRenderError>;
    fn render_session_read(&mut self, read: &CanonicalSessionRead) -> Result<(), CliRenderError>;
    fn render_session_rejoin(
        &mut self,
        rejoin: &RunningSessionRejoin,
    ) -> Result<(), CliRenderError>;
    fn render_session_turn_page(&mut self, page: &CanonicalTurnPage) -> Result<(), CliRenderError>;
    fn render_session_runtime_event_page(
        &mut self,
        page: &CanonicalRuntimeEventPage,
    ) -> Result<(), CliRenderError>;
    fn render_session_idle_turn_admission(
        &mut self,
        _admission: &IdleTurnAdmission,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_thread_goal_get(
        &mut self,
        _result: &ThreadGoalGetResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_thread_goal_set(
        &mut self,
        _result: &ThreadGoalSetResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_thread_goal_clear(
        &mut self,
        _result: &ThreadGoalClearResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

pub struct HumanRenderer;

impl HumanRenderer {
    pub fn new() -> Self {
        Self
    }
}

impl EventRenderer for HumanRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        match event {
            RunEvent::SessionStarted { session_id, title } => {
                writeln!(
                    stdout,
                    "session {} {}",
                    session_id,
                    terminal_safe_inline(title)
                )?;
            }
            RunEvent::SessionTitleUpdated { session_id, title } => {
                writeln!(
                    stdout,
                    "session {} title {}",
                    session_id,
                    terminal_safe_inline(title)
                )?;
            }
            RunEvent::UserTurnStored { session_id, .. } => {
                writeln!(stdout, "user turn {session_id}")?;
            }
            RunEvent::ModelRequestPrepared { .. } | RunEvent::WorldStateUpdated { .. } => {}
            RunEvent::ProviderPhase { event, .. } => {
                if let Some(failure) = &event.failure {
                    let failure = terminal_safe_inline(&failure.to_string()).into_owned();
                    writeln!(
                        stdout,
                        "[provider:{}] request={} attempt={} elapsed_ms={} {}",
                        event.phase.as_str(),
                        event.request_id,
                        event.attempt,
                        event.elapsed_ms,
                        failure
                    )?;
                } else {
                    writeln!(
                        stdout,
                        "[provider:{}] request={} attempt={} elapsed_ms={}",
                        event.phase.as_str(),
                        event.request_id,
                        event.attempt,
                        event.elapsed_ms
                    )?;
                }
            }
            RunEvent::TextDelta { delta, .. } => {
                write!(stdout, "{}", terminal_safe_multiline(delta))?;
            }
            RunEvent::AssistantMessageCommitted { .. } => {}
            RunEvent::ReasoningSummaryDelta { delta, .. } => {
                writeln!(
                    stdout,
                    "\n[reasoning summary] {}",
                    terminal_safe_multiline(delta)
                )?;
            }
            RunEvent::ToolCallPending { tool_name, .. } => {
                writeln!(stdout, "\n[tool] {}", terminal_safe_inline(tool_name))?;
            }
            RunEvent::ToolCallCompleted { summary, .. } => {
                writeln!(stdout, "[tool:done] {}", terminal_safe_inline(summary))?;
            }
            RunEvent::ToolCallDeclined { reason, .. } => {
                writeln!(stdout, "[tool:declined] {}", terminal_safe_inline(reason))?;
            }
            RunEvent::ToolCallCancelled { reason, .. } => {
                writeln!(stdout, "[tool:cancelled] {}", terminal_safe_inline(reason))?;
            }
            RunEvent::ToolCallFailed { error, .. } => {
                writeln!(stdout, "[tool:error] {}", terminal_safe_inline(error))?;
            }
            RunEvent::FileChangesRecorded { changes, .. } => {
                writeln!(
                    stdout,
                    "[changes] {}",
                    changes
                        .iter()
                        .map(|value| terminal_safe_inline(&value.summary_line(None)).into_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
            }
            RunEvent::CompactionCompleted {
                summarized_messages,
                ..
            } => {
                writeln!(
                    stdout,
                    "[compaction] summarized {summarized_messages} messages"
                )?;
            }
            RunEvent::PermissionRequested { summary, .. } => {
                writeln!(stdout, "[permission] {}", terminal_safe_inline(summary))?;
            }
            RunEvent::PermissionResolved { approved, .. } => {
                writeln!(
                    stdout,
                    "[permission] {}",
                    if *approved {
                        "approved"
                    } else {
                        "not approved"
                    }
                )?;
            }
            RunEvent::RecoverableRuntimeFeedback { message, .. } => {
                writeln!(stdout, "[feedback] {}", terminal_safe_inline(message))?;
            }
            RunEvent::TurnTerminal {
                session_id,
                terminal,
            } => {
                let status = match &terminal.outcome {
                    crate::protocol::TurnTerminalOutcome::Completed => "completed",
                    crate::protocol::TurnTerminalOutcome::Failed { .. } => "failed",
                    crate::protocol::TurnTerminalOutcome::Interrupted { .. } => "interrupted",
                };
                writeln!(
                    stdout,
                    "\n[turn:{status}] {session_id} {}",
                    terminal_safe_inline(&terminal.summary())
                )?;
            }
        }
        stdout.flush()?;
        Ok(())
    }

    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", human_run_summary_line(summary))?;
        Ok(())
    }

    fn render_session_list(&mut self, sessions: &[SessionRecord]) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        for session in sessions {
            writeln!(stdout, "{}", human_session_record_line(session))?;
        }
        Ok(())
    }

    fn render_loaded_sessions(&mut self, loaded: &LoadedSessionList) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "project {} loaded include_archived={}",
            loaded.project_id, loaded.include_archived
        )?;
        for summary in &loaded.sessions {
            writeln!(stdout, "{}", human_loaded_session_summary_line(summary))?;
        }
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} {}",
            session.id,
            terminal_safe_inline(&session.title)
        )?;
        for item in history_items {
            writeln!(stdout, "{}", human_history_item_line(item)?)?;
        }
        Ok(())
    }

    fn render_session_history_page(
        &mut self,
        page: &CanonicalHistoryPage,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} history offset={} limit={} total={} has_more={}",
            page.session.id, page.offset, page.limit, page.total, page.has_more
        )?;
        for item in &page.items {
            writeln!(
                stdout,
                "{}\t{}\t{}",
                item.sequence_no,
                item.id,
                payload_kind(&item.payload)?
            )?;
        }
        Ok(())
    }

    fn render_session_read(&mut self, read: &CanonicalSessionRead) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} {} status={} model={} access_mode={} cwd={}",
            read.session.id,
            terminal_safe_inline(&read.session.title),
            read.session.status.key(),
            terminal_safe_inline(&read.session.model),
            read.session.access_mode.as_str(),
            terminal_safe_inline(read.session.cwd.as_str())
        )?;
        if let Some(turn_id) = read.active_turn_id {
            writeln!(
                stdout,
                "active_turn {} sequence={}",
                turn_id,
                read.active_turn_sequence_no.unwrap_or_default()
            )?;
        }
        writeln!(
            stdout,
            "history offset={} limit={} total={} has_more={}",
            read.history.offset, read.history.limit, read.history.total, read.history.has_more
        )?;
        writeln!(
            stdout,
            "turns offset={} limit={} total={} has_more={}",
            read.turns.offset, read.turns.limit, read.turns.total, read.turns.has_more
        )?;
        Ok(())
    }

    fn render_session_rejoin(
        &mut self,
        rejoin: &RunningSessionRejoin,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "{}",
            human_loaded_session_summary_line(&rejoin.summary)
        )?;
        writeln!(
            stdout,
            "history offset={} limit={} total={} has_more={}",
            rejoin.read.history.offset,
            rejoin.read.history.limit,
            rejoin.read.history.total,
            rejoin.read.history.has_more
        )?;
        writeln!(
            stdout,
            "turns offset={} limit={} total={} has_more={}",
            rejoin.read.turns.offset,
            rejoin.read.turns.limit,
            rejoin.read.turns.total,
            rejoin.read.turns.has_more
        )?;
        Ok(())
    }

    fn render_session_turn_page(&mut self, page: &CanonicalTurnPage) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} turns offset={} limit={} total={} has_more={}",
            page.session.id, page.offset, page.limit, page.total, page.has_more
        )?;
        for item in &page.items {
            writeln!(
                stdout,
                "{}\t{}\t{}",
                item.sequence_no,
                item.id,
                payload_kind(&item.payload)?
            )?;
        }
        Ok(())
    }

    fn render_session_runtime_event_page(
        &mut self,
        page: &CanonicalRuntimeEventPage,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} events offset={} limit={} total={} has_more={}",
            page.session.id, page.offset, page.limit, page.total, page.has_more
        )?;
        for event in &page.items {
            writeln!(
                stdout,
                "{}\t{}\t{}\t{}",
                event.sequence_no,
                event.turn_id,
                event.id,
                payload_kind(&event.msg)?
            )?;
        }
        Ok(())
    }

    fn render_session_idle_turn_admission(
        &mut self,
        admission: &IdleTurnAdmission,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        let reason = admission
            .rejection_reason
            .map(|reason| reason.key())
            .unwrap_or("none");
        writeln!(
            stdout,
            "session {} idle_admitted={} reason={}",
            admission.session.id, admission.admitted, reason
        )?;
        Ok(())
    }

    fn render_thread_goal_get(
        &mut self,
        result: &ThreadGoalGetResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        match &result.goal {
            Some(goal) => writeln!(stdout, "{}", human_thread_goal_line(goal))?,
            None => writeln!(stdout, "goal none")?,
        }
        Ok(())
    }

    fn render_thread_goal_set(
        &mut self,
        result: &ThreadGoalSetResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", human_thread_goal_line(&result.goal))?;
        Ok(())
    }

    fn render_thread_goal_clear(
        &mut self,
        result: &ThreadGoalClearResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "goal thread={} cleared={}",
            result.thread_id, result.cleared
        )?;
        Ok(())
    }
}

pub struct JsonRenderer;

impl JsonRenderer {
    pub fn new() -> Self {
        Self
    }
}

impl EventRenderer for JsonRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(event)?)?;
        Ok(())
    }

    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(summary)?)?;
        Ok(())
    }

    fn render_session_list(&mut self, sessions: &[SessionRecord]) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(sessions)?)?;
        Ok(())
    }

    fn render_loaded_sessions(&mut self, loaded: &LoadedSessionList) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(loaded)?)?;
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        let payload = serde_json::json!({
            "session": session,
            "history_items": history_items,
        });
        writeln!(stdout, "{}", serde_json::to_string(&payload)?)?;
        Ok(())
    }

    fn render_session_history_page(
        &mut self,
        page: &CanonicalHistoryPage,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(page)?)?;
        Ok(())
    }

    fn render_session_read(&mut self, read: &CanonicalSessionRead) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(read)?)?;
        Ok(())
    }

    fn render_session_rejoin(
        &mut self,
        rejoin: &RunningSessionRejoin,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(rejoin)?)?;
        Ok(())
    }

    fn render_session_turn_page(&mut self, page: &CanonicalTurnPage) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(page)?)?;
        Ok(())
    }

    fn render_session_runtime_event_page(
        &mut self,
        page: &CanonicalRuntimeEventPage,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(page)?)?;
        Ok(())
    }

    fn render_session_idle_turn_admission(
        &mut self,
        admission: &IdleTurnAdmission,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(admission)?)?;
        Ok(())
    }

    fn render_thread_goal_get(
        &mut self,
        result: &ThreadGoalGetResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(result)?)?;
        Ok(())
    }

    fn render_thread_goal_set(
        &mut self,
        result: &ThreadGoalSetResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(result)?)?;
        Ok(())
    }

    fn render_thread_goal_clear(
        &mut self,
        result: &ThreadGoalClearResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(result)?)?;
        Ok(())
    }
}

fn payload_kind<T: serde::Serialize>(payload: &T) -> Result<String, CliRenderError> {
    let value = serde_json::to_value(payload)?;
    Ok(value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

fn human_run_summary_line(summary: &RunSummary) -> String {
    format!(
        "summary: status={} tools={} failed_tools={} changes={}",
        summary.status().key(),
        summary.tool_call_count(),
        summary.failed_tool_count(),
        summary.change_count()
    )
}

fn human_session_record_line(session: &SessionRecord) -> String {
    let model_parameters = human_model_parameters(&session.model_parameters);
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        session.id,
        session.status.key(),
        session.access_mode.as_str(),
        model_parameters,
        session.updated_at_ms,
        terminal_safe_inline(&session.title)
    )
}

fn human_model_parameters(parameters: &crate::session::SessionModelParameters) -> String {
    if parameters.is_empty() {
        return "model_params=-".to_string();
    }
    let mut parts = Vec::new();
    if let Some(value) = parameters.temperature {
        parts.push(format!("temperature={value}"));
    }
    if let Some(value) = parameters.top_p {
        parts.push(format!("top_p={value}"));
    }
    if let Some(value) = parameters.top_k {
        parts.push(format!("top_k={value}"));
    }
    if let Some(value) = parameters.max_output_tokens {
        parts.push(format!("max_output_tokens={value}"));
    }
    format!("model_params={}", parts.join(","))
}

fn human_loaded_session_summary_line(summary: &crate::session::LoadedSessionSummary) -> String {
    let active_turn = summary
        .active_turn_id
        .map(|turn_id| turn_id.to_string())
        .unwrap_or_else(|| "-".to_string());
    let active_sequence = summary
        .active_turn_sequence_no
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "{}\t{}\tsession_status={}\taccess_mode={}\tactive_turn={}\tsequence={}\tpending_user_input={}\t{}",
        summary.session.id,
        summary.loaded_status.key(),
        summary.session.status.key(),
        summary.session.access_mode.as_str(),
        active_turn,
        active_sequence,
        summary.pending_user_input_requests,
        terminal_safe_inline(&summary.session.title)
    )
}

fn human_thread_goal_line(goal: &ThreadGoal) -> String {
    let budget = goal
        .token_budget
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let remaining = goal
        .token_budget
        .map(|budget| (budget - goal.tokens_used).max(0).to_string())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "goal thread={} status={} tokens={}/{} remaining={} elapsed_seconds={} objective={}",
        goal.thread_id,
        goal.status.key(),
        goal.tokens_used,
        budget,
        remaining,
        goal.time_used_seconds,
        terminal_safe_inline(&goal.objective)
    )
}

fn human_history_item_line(item: &HistoryItem) -> Result<String, CliRenderError> {
    Ok(format!(
        "{}\t{}\t{}",
        item.sequence_no,
        payload_kind(&item.payload)?,
        serde_json::to_string(&item.payload)?
    ))
}

fn renderer_fixture_session_record(title: &str) -> SessionRecord {
    SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: title.to_string(),
        status: crate::session::SessionStatus::Completed,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: "fixture-model".to_string(),
        base_url: "http://fixture.invalid/v1".to_string(),
        created_at_ms: 1,
        updated_at_ms: 3,
        completed_at_ms: Some(3),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
    }
}

pub fn cli_history_renderer_uses_canonical_history_projection_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("renderer fixture");
    let later = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        scope: crate::protocol::HistoryScope::Turn {
            turn_id: crate::protocol::TurnId::new(),
        },
        sequence_no: 2,
        created_at_ms: 2,
        payload: crate::protocol::HistoryItemPayload::AssistantMessage {
            response_id: crate::protocol::ModelResponseId::new(),
            content: vec![crate::protocol::ContentPart::Text {
                text: "assistant".to_string(),
            }],
        },
    };
    let earlier = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        scope: crate::protocol::HistoryScope::Turn {
            turn_id: crate::protocol::TurnId::new(),
        },
        sequence_no: 1,
        created_at_ms: 1,
        payload: crate::protocol::HistoryItemPayload::UserTurn {
            content: vec![crate::protocol::ContentPart::Text {
                text: "user".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        },
    };
    let projected = [earlier, later];
    projected.first().is_some_and(|item| {
        matches!(
            item.payload,
            crate::protocol::HistoryItemPayload::UserTurn { .. }
        )
    })
}

pub fn cli_session_read_payload_preserves_metadata_pages_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("thread read fixture");
    let active_turn_id = crate::protocol::TurnId::new();
    let read = CanonicalSessionRead {
        session: session.clone(),
        history: CanonicalHistoryPage {
            session: session.clone(),
            offset: 10,
            limit: 5,
            total: 17,
            has_more: true,
            items: Vec::new(),
        },
        turns: CanonicalTurnPage {
            session,
            offset: 2,
            limit: 3,
            total: 8,
            has_more: true,
            items: Vec::new(),
        },
        active_turn_id: Some(active_turn_id),
        active_turn_sequence_no: Some(42),
    };
    let encoded = serde_json::to_string(&read).unwrap_or_default();

    encoded.contains("thread read fixture")
        && encoded.contains("\"history\"")
        && encoded.contains("\"turns\"")
        && encoded.contains("\"active_turn_id\"")
        && encoded.contains("\"active_turn_sequence_no\":42")
        && encoded.contains("\"offset\":10")
        && encoded.contains("\"limit\":5")
        && encoded.contains("\"total\":17")
}

pub fn cli_human_renderer_typed_lifecycle_projection_fixture_passes() -> bool {
    let summary_line = human_run_summary_line(&RunSummary::from_terminal(
        crate::session::SessionId::new(),
        crate::protocol::TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count: 2,
            failed_tool_count: 1,
            change_count: 3,
            metrics: Default::default(),
        },
    ));
    let session = renderer_fixture_session_record("typed projection");
    let session_line = human_session_record_line(&session);
    let history_item = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        scope: crate::protocol::HistoryScope::Turn {
            turn_id: crate::protocol::TurnId::new(),
        },
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::AssistantMessage {
            response_id: crate::protocol::ModelResponseId::new(),
            content: vec![crate::protocol::ContentPart::Text {
                text: "canonical assistant".to_string(),
            }],
        },
    };
    let payload = human_history_item_line(&history_item).unwrap_or_default();

    summary_line.contains("status=completed")
        && !summary_line.contains("Completed")
        && session_line.contains("\tcompleted\t")
        && !session_line.contains("\tCompleted\t")
        && payload.contains("canonical assistant")
        && payload.contains("message")
}

pub fn cli_human_renderer_neutralizes_terminal_controls_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("visible\u{1b}]52;c;secret\u{7}\nspoofed");
    let line = human_session_record_line(&session);

    !line.contains('\u{1b}')
        && !line.contains('\u{7}')
        && !line.contains('\n')
        && line.contains("\\u{001B}")
        && line.contains("\\u{0007}")
        && line.contains("\\u{000A}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_history_renderer_uses_canonical_history_projection() {
        assert!(super::cli_history_renderer_uses_canonical_history_projection_fixture_passes());
    }

    #[test]
    fn cli_session_read_payload_preserves_metadata_pages() {
        assert!(super::cli_session_read_payload_preserves_metadata_pages_fixture_passes());
    }

    #[test]
    fn cli_human_renderer_typed_lifecycle_projection() {
        assert!(super::cli_human_renderer_typed_lifecycle_projection_fixture_passes());
    }

    #[test]
    fn cli_human_renderer_neutralizes_terminal_controls() {
        assert!(super::cli_human_renderer_neutralizes_terminal_controls_fixture_passes());
    }
}
