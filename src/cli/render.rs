use crate::error::CliRenderError;
use crate::protocol::{HistoryItem, HistoryItemPayload};
use crate::session::{
    CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead, CanonicalTurnPage,
    IdleTurnAdmission, LoadedSessionList, MessagePart, PartKind, RunEvent, RunSummary,
    RunningSessionRejoin, SessionCompactResult, SessionMemoryModeUpdate, SessionRecord,
    SessionStateSnapshot, Transcript, transcript_from_history_items,
};

const CURRENT_PROVIDER_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const CURRENT_PROVIDER_BASE_URL: &str = "http://127.0.0.1:1234";

pub trait EventRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError>;
    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError>;
    fn render_session_list(&mut self, sessions: &[SessionRecord]) -> Result<(), CliRenderError>;
    fn render_loaded_sessions(&mut self, loaded: &LoadedSessionList) -> Result<(), CliRenderError>;
    fn render_session_show(&mut self, transcript: &Transcript) -> Result<(), CliRenderError>;
    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
        show_reasoning: bool,
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
    fn render_session_compact_result(
        &mut self,
        result: &SessionCompactResult,
    ) -> Result<(), CliRenderError>;
    fn render_session_memory_mode_update(
        &mut self,
        update: &SessionMemoryModeUpdate,
    ) -> Result<(), CliRenderError>;
    fn render_session_idle_turn_admission(
        &mut self,
        _admission: &IdleTurnAdmission,
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
                writeln!(stdout, "session {} {}", session_id, title)?;
            }
            RunEvent::SessionTitleUpdated { session_id, title } => {
                writeln!(stdout, "session {} title {}", session_id, title)?;
            }
            RunEvent::UserMessageStored { message_id } => {
                writeln!(stdout, "user {}", message_id)?;
            }
            RunEvent::UserTurnStored { message_id, .. } => {
                writeln!(stdout, "user turn {}", message_id)?;
            }
            RunEvent::AssistantStarted { model, .. } => {
                writeln!(stdout, "assistant ({model})")?;
            }
            RunEvent::ControlEnvelopePrepared { .. }
            | RunEvent::ModelRequestPrepared { .. }
            | RunEvent::LifecycleGuardUpdated { .. } => {}
            RunEvent::TextDelta { delta, .. } => {
                write!(stdout, "{delta}")?;
            }
            RunEvent::ReasoningDelta { delta, .. } => {
                writeln!(stdout, "\n[reasoning] {delta}")?;
            }
            RunEvent::ToolCallPending { title, .. } => {
                writeln!(stdout, "\n[tool] {title}")?;
            }
            RunEvent::ToolCallCompleted { summary, .. } => {
                writeln!(stdout, "[tool:done] {summary}")?;
            }
            RunEvent::ToolCallFailed { error, .. } => {
                writeln!(stdout, "[tool:error] {error}")?;
            }
            RunEvent::ToolProposalRejected { .. }
            | RunEvent::CandidateRepairEditRecorded { .. } => {}
            RunEvent::FileChangesRecorded { changes, .. } => {
                writeln!(
                    stdout,
                    "[changes] {}",
                    changes
                        .iter()
                        .map(|value| value.summary_line(None))
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
                writeln!(stdout, "[permission] {summary}")?;
            }
            RunEvent::PermissionResolved { approved, .. } => {
                writeln!(
                    stdout,
                    "[permission] {}",
                    if *approved { "approved" } else { "denied" }
                )?;
            }
            RunEvent::RetryScheduled {
                attempt,
                message,
                next_retry_at_ms,
                ..
            } => {
                writeln!(
                    stdout,
                    "[retry] attempt={} next_retry_at_ms={} {}",
                    attempt, next_retry_at_ms, message
                )?;
            }
            RunEvent::RecoverableRuntimeFeedback { message, .. } => {
                writeln!(stdout, "[feedback] {message}")?;
            }
            RunEvent::StateUpdated { state, .. } => {
                write!(stdout, "{}", human_state_update_line(state))?;
                if let Some(reason) = &state.completion.blocked_reason {
                    write!(stdout, " blocked={reason}")?;
                }
                if let Some(summary) = &state.completion.route_contract_summary {
                    write!(stdout, " docs_contract={summary}")?;
                }
                if let Some(failure) = &state.failure {
                    write!(stdout, " failure={}", failure.summary)?;
                }
                writeln!(stdout)?;
            }
            RunEvent::SessionCompleted { session_id, .. } => {
                writeln!(stdout, "\n[completed] {session_id}")?;
            }
            RunEvent::SessionAwaitingUser { session_id, .. } => {
                writeln!(stdout, "\n[awaiting-user] {session_id}")?;
            }
            RunEvent::SessionInterrupted { reason, .. } => {
                writeln!(stdout, "\n[interrupted] {reason}")?;
            }
            RunEvent::SessionFailed { message, .. } => {
                writeln!(stdout, "\n[failed] {message}")?;
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

    fn render_session_show(&mut self, transcript: &Transcript) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} {}",
            transcript.session.id, transcript.session.title
        )?;
        for message in &transcript.messages {
            writeln!(stdout, "{}:", message.record.role.key())?;
            for part in &message.parts {
                writeln!(
                    stdout,
                    "  {} {}",
                    part.kind.key(),
                    human_message_part_payload(&part.payload)?
                )?;
            }
        }
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
        show_reasoning: bool,
    ) -> Result<(), CliRenderError> {
        let transcript = transcript_for_history_render(session, history_items, show_reasoning);
        self.render_session_show(&transcript)
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
            read.session.title,
            read.session.status.key(),
            read.session.model,
            read.session.access_mode.as_str(),
            read.session.cwd
        )?;
        writeln!(
            stdout,
            "state route={} phase={}",
            read.state.route.key(),
            read.state.process_phase.key()
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

    fn render_session_compact_result(
        &mut self,
        result: &SessionCompactResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} compacted item={} summarized={} retained={}",
            result.session.id,
            result.compaction_item_id,
            result.summarized_history_items,
            result.retained_history_items
        )?;
        Ok(())
    }

    fn render_session_memory_mode_update(
        &mut self,
        update: &SessionMemoryModeUpdate,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(
            stdout,
            "session {} memory={} changed={}",
            update.session.id,
            update.mode.key(),
            update.changed
        )?;
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

    fn render_session_show(&mut self, transcript: &Transcript) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(transcript)?)?;
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
        show_reasoning: bool,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        let transcript = transcript_for_history_render(session, history_items, show_reasoning);
        let visible_history_items = history_items_for_render_payload(history_items, show_reasoning);
        let mut payload = serde_json::to_value(transcript)?;
        if let serde_json::Value::Object(object) = &mut payload {
            object.insert(
                "history_items".to_string(),
                serde_json::to_value(&visible_history_items)?,
            );
        }
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

    fn render_session_compact_result(
        &mut self,
        result: &SessionCompactResult,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(result)?)?;
        Ok(())
    }

    fn render_session_memory_mode_update(
        &mut self,
        update: &SessionMemoryModeUpdate,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(update)?)?;
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
}

fn payload_kind<T: serde::Serialize>(payload: &T) -> Result<String, CliRenderError> {
    let value = serde_json::to_value(payload)?;
    Ok(value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

fn transcript_for_history_render(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    show_reasoning: bool,
) -> Transcript {
    let transcript = transcript_from_history_items(session, history_items);
    if show_reasoning {
        transcript
    } else {
        strip_reasoning(transcript)
    }
}

fn history_items_for_render_payload(
    history_items: &[HistoryItem],
    show_reasoning: bool,
) -> Vec<HistoryItem> {
    if show_reasoning {
        return history_items.to_vec();
    }
    history_items
        .iter()
        .filter(|item| !matches!(&item.payload, HistoryItemPayload::Reasoning { .. }))
        .cloned()
        .collect()
}

fn human_state_update_line(state: &SessionStateSnapshot) -> String {
    format!(
        "[state] route={} phase={}",
        state.route.key(),
        state.process_phase.key()
    )
}

fn human_run_summary_line(summary: &RunSummary) -> String {
    format!(
        "summary: status={} tools={} failed_tools={} changes={}",
        summary.status.key(),
        summary.tool_call_count,
        summary.failed_tool_count,
        summary.change_count
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
        session.title
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
        summary.session.title
    )
}

fn human_message_part_payload(part: &MessagePart) -> Result<String, CliRenderError> {
    Ok(serde_json::to_string(part)?)
}

fn renderer_fixture_session_record(title: &str) -> SessionRecord {
    SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: title.to_string(),
        status: crate::session::SessionStatus::Completed,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: CURRENT_PROVIDER_MODEL.to_string(),
        base_url: CURRENT_PROVIDER_BASE_URL.to_string(),
        created_at_ms: 1,
        updated_at_ms: 3,
        completed_at_ms: Some(3),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
    }
}

pub fn cli_history_renderer_uses_canonical_transcript_projection_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("renderer fixture");
    let later = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id: crate::protocol::TurnId::new(),
        sequence_no: 2,
        created_at_ms: 2,
        payload: crate::protocol::HistoryItemPayload::Message {
            message_id: None,
            role: crate::session::MessageRole::Assistant,
            content: vec![crate::protocol::ContentPart::Text {
                text: "assistant".to_string(),
            }],
        },
    };
    let earlier = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id: crate::protocol::TurnId::new(),
        sequence_no: 1,
        created_at_ms: 1,
        payload: crate::protocol::HistoryItemPayload::Message {
            message_id: None,
            role: crate::session::MessageRole::User,
            content: vec![crate::protocol::ContentPart::Text {
                text: "user".to_string(),
            }],
        },
    };
    let projected = transcript_for_history_render(&session, &[later, earlier], true);
    projected
        .messages
        .first()
        .is_some_and(|message| message.record.role == crate::session::MessageRole::User)
}

pub fn cli_history_renderer_ignores_compatibility_transcript_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("renderer fixture");
    let turn_id = crate::protocol::TurnId::new();
    let items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: crate::protocol::HistoryItemPayload::Message {
                message_id: None,
                role: crate::session::MessageRole::User,
                content: vec![crate::protocol::ContentPart::Text {
                    text: "canonical user".to_string(),
                }],
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: crate::protocol::HistoryItemPayload::Reasoning {
                text: "internal reasoning".to_string(),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: crate::protocol::HistoryItemPayload::Message {
                message_id: None,
                role: crate::session::MessageRole::Assistant,
                content: vec![crate::protocol::ContentPart::Text {
                    text: "canonical assistant".to_string(),
                }],
            },
        },
    ];
    let projected = transcript_for_history_render(&session, &items, false);
    let rendered = serde_json::to_string(&projected).unwrap_or_default();

    rendered.contains("canonical user")
        && rendered.contains("canonical assistant")
        && !rendered.contains("internal reasoning")
        && projected.messages.iter().all(|message| {
            message
                .parts
                .iter()
                .all(|part| !matches!(part.payload, MessagePart::Reasoning(_)))
        })
}

pub fn cli_json_history_renderer_respects_reasoning_visibility_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("renderer fixture");
    let turn_id = crate::protocol::TurnId::new();
    let items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: crate::session::MessageRole::User,
                content: vec![crate::protocol::ContentPart::Text {
                    text: "canonical user".to_string(),
                }],
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::Reasoning {
                text: "internal reasoning".to_string(),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: crate::session::MessageRole::Assistant,
                content: vec![crate::protocol::ContentPart::Text {
                    text: "canonical assistant".to_string(),
                }],
            },
        },
    ];
    let hidden_payload = history_items_for_render_payload(&items, false);
    let visible_payload = history_items_for_render_payload(&items, true);
    let hidden_json = serde_json::to_string(&hidden_payload).unwrap_or_default();
    let visible_json = serde_json::to_string(&visible_payload).unwrap_or_default();

    hidden_payload.len() == 2
        && visible_payload.len() == 3
        && !hidden_json.contains("internal reasoning")
        && visible_json.contains("internal reasoning")
}

pub fn cli_session_read_payload_preserves_metadata_pages_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("thread read fixture");
    let active_turn_id = crate::protocol::TurnId::new();
    let read = CanonicalSessionRead {
        session: session.clone(),
        state: SessionStateSnapshot::default(),
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

pub fn cli_renderer_current_provider_profile_fixture_passes() -> bool {
    let session = renderer_fixture_session_record("cli_renderer_fixture_current_provider_profile");
    session.model == CURRENT_PROVIDER_MODEL && session.base_url == CURRENT_PROVIDER_BASE_URL
}

pub fn cli_human_renderer_typed_lifecycle_projection_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: crate::session::TaskRoute::Docs,
        process_phase: crate::session::ProcessPhase::Verify,
        ..SessionStateSnapshot::default()
    };
    state.completion.blocked_reason = Some("verification pending".to_string());
    let state_line = human_state_update_line(&state);
    let summary_line = human_run_summary_line(&RunSummary {
        session_id: crate::session::SessionId::new(),
        assistant_message_id: None,
        status: crate::session::SessionStatus::AwaitingUser,
        finish_reason: None,
        tool_call_count: 2,
        failed_tool_count: 1,
        change_count: 3,
        metrics: Default::default(),
    });
    let session = renderer_fixture_session_record("typed projection");
    let session_line = human_session_record_line(&session);
    let role_key = crate::session::MessageRole::Assistant.key();
    let text_payload = MessagePart::Text(crate::session::TextPart {
        text: "canonical assistant".to_string(),
    });
    let payload = human_message_part_payload(&text_payload).unwrap_or_default();

    state_line == "[state] route=docs phase=verify"
        && !state_line.contains("Docs")
        && !state_line.contains("Verify")
        && summary_line.contains("status=awaiting_user")
        && !summary_line.contains("AwaitingUser")
        && session_line.contains("\tcompleted\t")
        && !session_line.contains("\tCompleted\t")
        && role_key == "assistant"
        && payload.contains("canonical assistant")
        && !payload.contains("TextPart")
}

fn strip_reasoning(mut transcript: Transcript) -> Transcript {
    for message in &mut transcript.messages {
        message
            .parts
            .retain(|part| !matches!(part.kind, PartKind::Reasoning));
    }
    transcript
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_history_renderer_uses_canonical_transcript_projection() {
        assert!(super::cli_history_renderer_uses_canonical_transcript_projection_fixture_passes());
    }

    #[test]
    fn cli_history_renderer_ignores_compatibility_transcript() {
        assert!(super::cli_history_renderer_ignores_compatibility_transcript_fixture_passes());
    }

    #[test]
    fn cli_json_history_renderer_respects_reasoning_visibility() {
        assert!(super::cli_json_history_renderer_respects_reasoning_visibility_fixture_passes());
    }

    #[test]
    fn cli_session_read_payload_preserves_metadata_pages() {
        assert!(super::cli_session_read_payload_preserves_metadata_pages_fixture_passes());
    }

    #[test]
    fn cli_human_renderer_typed_lifecycle_projection() {
        assert!(super::cli_human_renderer_typed_lifecycle_projection_fixture_passes());
    }
}
