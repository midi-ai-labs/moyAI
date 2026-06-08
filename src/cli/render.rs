use crate::error::CliRenderError;
use crate::protocol::{HistoryItem, HistoryItemPayload};
use crate::session::{
    MessagePart, PartKind, RunEvent, RunSummary, SessionRecord, SessionStateSnapshot, Transcript,
    transcript_from_history_items,
};

const CURRENT_PROVIDER_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const CURRENT_PROVIDER_BASE_URL: &str = "http://127.0.0.1:1234";

pub trait EventRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError>;
    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError>;
    fn render_session_list(&mut self, sessions: &[SessionRecord]) -> Result<(), CliRenderError>;
    fn render_session_show(&mut self, transcript: &Transcript) -> Result<(), CliRenderError>;
    fn render_session_history_items(
        &mut self,
        session: &SessionRecord,
        history_items: &[HistoryItem],
        show_reasoning: bool,
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
    format!(
        "{}\t{}\t{}\t{}",
        session.id,
        session.status.key(),
        session.updated_at_ms,
        session.title
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
    fn cli_human_renderer_typed_lifecycle_projection() {
        assert!(super::cli_human_renderer_typed_lifecycle_projection_fixture_passes());
    }
}
