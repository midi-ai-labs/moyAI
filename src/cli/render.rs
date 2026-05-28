use crate::error::CliRenderError;
use crate::protocol::HistoryItem;
use crate::session::{
    RunEvent, RunSummary, SessionRecord, Transcript, transcript_from_history_items,
};

pub trait EventRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError>;
    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError>;
    fn render_session_list(&mut self, sessions: &[SessionRecord]) -> Result<(), CliRenderError>;
    fn render_session_show(&mut self, transcript: &Transcript) -> Result<(), CliRenderError>;
    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
        transcript: &Transcript,
    ) -> Result<(), CliRenderError>;
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
            RunEvent::ControlEnvelopePrepared { .. } | RunEvent::ModelRequestPrepared { .. } => {}
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
                write!(
                    stdout,
                    "[state] route={:?} phase={:?}",
                    state.route, state.process_phase
                )?;
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
        writeln!(
            stdout,
            "summary: status={:?} tools={} failed_tools={} changes={}",
            summary.status,
            summary.tool_call_count,
            summary.failed_tool_count,
            summary.change_count
        )?;
        Ok(())
    }

    fn render_session_list(&mut self, sessions: &[SessionRecord]) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        for session in sessions {
            writeln!(
                stdout,
                "{}\t{:?}\t{}\t{}",
                session.id, session.status, session.updated_at_ms, session.title
            )?;
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
            writeln!(stdout, "{:?}:", message.record.role)?;
            for part in &message.parts {
                writeln!(stdout, "  {:?}", part.payload)?;
            }
        }
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
        transcript: &Transcript,
    ) -> Result<(), CliRenderError> {
        let transcript = transcript_for_history_render(session, history_items, transcript);
        self.render_session_show(&transcript)
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
        transcript: &Transcript,
    ) -> Result<(), CliRenderError> {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        let transcript = transcript_for_history_render(session, history_items, transcript);
        let mut payload = serde_json::to_value(transcript)?;
        if let serde_json::Value::Object(object) = &mut payload {
            object.insert(
                "history_items".to_string(),
                serde_json::to_value(history_items)?,
            );
        }
        writeln!(stdout, "{}", serde_json::to_string(&payload)?)?;
        Ok(())
    }
}

fn transcript_for_history_render(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    transcript: &Transcript,
) -> Transcript {
    if transcript.messages.is_empty() {
        transcript_from_history_items(session, history_items)
    } else {
        transcript.clone()
    }
}

pub(crate) fn cli_history_renderer_uses_canonical_transcript_projection_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "renderer fixture".to_string(),
        status: crate::session::SessionStatus::Completed,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: "model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 3,
        completed_at_ms: Some(3),
    };
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
    let empty = Transcript {
        session: session.clone(),
        messages: Vec::new(),
    };
    let projected = transcript_for_history_render(&session, &[later, earlier], &empty);
    projected
        .messages
        .first()
        .is_some_and(|message| message.record.role == crate::session::MessageRole::User)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_history_renderer_uses_canonical_transcript_projection() {
        assert!(super::cli_history_renderer_uses_canonical_transcript_projection_fixture_passes());
    }
}
